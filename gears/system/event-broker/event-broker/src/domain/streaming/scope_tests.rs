//! Pure: an `AccessScope` in, a compiled scope and a yes or no per event out.
//! No PDP, no storage, no runtime.

use std::collections::HashSet;
use std::sync::Arc;
use std::thread;

use authz_resolver_sdk::TenantMode;
use chrono::DateTime;
use gts::{GtsId, GtsIdPattern, GtsTypeId};
use serde_json::json;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, ScopeValue, pep_properties};
use uuid::Uuid;

use crate::domain::model::Event;

use super::{
    AdmittedSubjectTypes, CONSUME_PROPS, CompiledConstraint, CompiledScope, DropReason,
    EVENT_SUBJECT_TYPE, EVENT_TYPE, EventPredicate, ScopeField, ScopeNotEnforceable,
    SubjectTypesOf, TenantCheck, TenantSetInterner, TypePlan,
};

const T1: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0001);
const T2: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0002);
const T3: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0003);
const T4: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0004);
const U1: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0011);
const U2: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0012);

const A: &str = "gts.cf.core.events.event.v1~x.eb.orders.created.v1~";
const B: &str = "gts.cf.core.events.event.v1~x.eb.orders.shipped.v1~";
const S: &str = "gts.x.eb.orders.subject.v1~";
const S2: &str = "gts.x.eb.invoices.subject.v1~";
/// A subject type derived from `S`.
const S_DERIVED: &str = "gts.x.eb.orders.subject.v1~x.eb.orders.special.v1~";
/// A minor version of `S`.
const S_MINOR: &str = "gts.x.eb.orders.subject.v1.2~";
/// Another subject type derived from `S`, beside `S_DERIVED`.
const S_RUSH: &str = "gts.x.eb.orders.subject.v1~x.eb.orders.rush.v1~";
/// A pattern every subject type here matches.
const ANY_X: &str = "gts.x.*";

const TENANT: &str = pep_properties::OWNER_TENANT_ID;

fn type_id(raw: &str) -> GtsTypeId {
    crate::test_support::event_type_id(raw)
}

fn types(raw: &[&str]) -> HashSet<GtsTypeId> {
    raw.iter().map(|r| type_id(r)).collect()
}

/// The parsed types a constraint admits by subject type, in the order given.
fn parsed(raw: &[&str]) -> Box<[GtsId]> {
    raw.iter().map(|r| GtsId::try_new(r).unwrap()).collect()
}

fn pattern(raw: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(raw).unwrap()
}

fn tenants(ids: &[Uuid]) -> Arc<HashSet<Uuid>> {
    Arc::new(ids.iter().copied().collect())
}

fn text(s: &str) -> ScopeValue {
    ScopeValue::String(s.to_owned())
}

fn scope(constraints: Vec<Vec<ScopeFilter>>) -> AccessScope {
    AccessScope::from_constraints(constraints.into_iter().map(ScopeConstraint::new).collect())
}

fn compile(scope: &AccessScope, mode: &TenantMode) -> Result<CompiledScope, ScopeNotEnforceable> {
    CompiledScope::compile(scope, mode, &TenantSetInterner::new())
}

fn event(tenant: Uuid, event_type: &str, subject_type: &str) -> Event {
    Event {
        id: Uuid::nil(),
        r#type: type_id(event_type),
        topic: GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.orders.acme.v1").unwrap(),
        subject_tenant: tenant,
        source: "scope-test".to_owned(),
        subject: "order".to_owned(),
        subject_type: type_id(subject_type),
        subject_owner: None,
        occurred_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        trace_parent: None,
        data: json!({}),
        meta: None,
        ingest: None,
        partition: Some(0),
        sequence: None,
        sequence_time: None,
    }
}

fn not_enforceable(dropped: Vec<DropReason>) -> Result<CompiledScope, ScopeNotEnforceable> {
    Err(ScopeNotEnforceable { dropped })
}

#[test]
fn an_eq_tenant_filter_compiles_to_one_tenant() {
    let scope = scope(vec![vec![ScopeFilter::eq(TENANT, T1)]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::One(T1),
                rest: Box::new([]),
            }],
        })
    );
}

#[test]
fn an_in_tenant_filter_with_one_value_compiles_to_one_tenant() {
    let scope = scope(vec![vec![ScopeFilter::in_uuids(TENANT, vec![T1, T1])]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::One(T1),
                rest: Box::new([]),
            }],
        })
    );
}

#[test]
fn an_in_tenant_filter_with_several_values_compiles_to_an_interned_set() {
    let scope = scope(vec![vec![ScopeFilter::in_uuids(TENANT, vec![T1, T2])]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::Set(tenants(&[T1, T2])),
                rest: Box::new([]),
            }],
        })
    );
}

#[test]
fn a_tenant_given_as_a_uuid_string_is_read_as_that_tenant() {
    let scope = scope(vec![vec![ScopeFilter::eq(TENANT, T1.to_string())]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::One(T1),
                rest: Box::new([]),
            }],
        })
    );
}

#[test]
fn two_tenant_filters_in_one_constraint_keep_only_their_common_tenants() {
    let sets = scope(vec![vec![
        ScopeFilter::in_uuids(TENANT, vec![T1, T2, T3]),
        ScopeFilter::in_uuids(TENANT, vec![T2, T3, T4]),
    ]]);
    assert_eq!(
        compile(&sets, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::Set(tenants(&[T2, T3])),
                rest: Box::new([]),
            }],
        }),
        "two sets"
    );

    let one_and_set = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::in_uuids(TENANT, vec![T1, T2]),
    ]]);
    assert_eq!(
        compile(&one_and_set, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::One(T1),
                rest: Box::new([]),
            }],
        }),
        "one tenant and a set"
    );
}

#[test]
fn tenant_filters_with_no_common_tenant_drop_their_constraint() {
    let scope = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(TENANT, T2),
    ]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        not_enforceable(vec![DropReason::MatchesNothing])
    );
}

/// `AccessScope` removes a constraint holding an empty `in` list when it is
/// built, so `compile` never sees it: alone, it leaves nothing to enforce.
#[test]
fn an_empty_in_list_leaves_nothing_to_enforce() {
    let scope = scope(vec![vec![
        ScopeFilter::r#in(EVENT_TYPE, vec![]),
        ScopeFilter::eq(TENANT, T1),
    ]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        not_enforceable(vec![])
    );
}

#[test]
fn type_and_subject_type_filters_become_predicates_of_the_constraint() {
    let scope = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::r#in(EVENT_TYPE, vec![text(A), text(B)]),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S)),
    ]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::One(T1),
                rest: Box::new([
                    EventPredicate::TypeIn(types(&[A, B])),
                    EventPredicate::SubjectTypeIn(types(&[S])),
                ]),
            }],
        })
    );
}

#[test]
fn a_constraint_without_a_tenant_filter_reads_any_tenant_under_root_only() {
    let scope = scope(vec![vec![ScopeFilter::r#in(EVENT_TYPE, vec![text(A)])]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::Any,
                rest: Box::new([EventPredicate::TypeIn(types(&[A]))]),
            }],
        })
    );
}

#[test]
fn a_subtree_constraint_without_a_tenant_filter_is_dropped() {
    let scope = scope(vec![vec![ScopeFilter::r#in(EVENT_TYPE, vec![text(A)])]]);

    assert_eq!(
        compile(&scope, &TenantMode::Subtree),
        not_enforceable(vec![DropReason::NoTenantFilter])
    );
}

#[test]
fn a_subtree_constraint_with_a_tenant_filter_is_kept() {
    let scope = scope(vec![vec![ScopeFilter::in_uuids(TENANT, vec![T1, T2])]]);

    assert_eq!(
        compile(&scope, &TenantMode::Subtree),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::Set(tenants(&[T1, T2])),
                rest: Box::new([]),
            }],
        })
    );
}

#[test]
fn an_owner_filter_becomes_an_owner_predicate_of_the_constraint() {
    let scope = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::in_uuids(pep_properties::OWNER_ID, vec![U1, U2]),
    ]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::One(T1),
                rest: Box::new([EventPredicate::OwnerIn(HashSet::from([U1, U2]))]),
            }],
        })
    );
}

#[test]
fn an_owner_filter_admits_an_event_whose_owner_it_names() {
    let compiled = compile(
        &scope(vec![vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::eq(pep_properties::OWNER_ID, U1),
        ]]),
        &TenantMode::RootOnly,
    )
    .unwrap();

    for (label, owner, expected) in [
        ("the named owner", Some(U1), true),
        ("another owner", Some(U2), false),
        ("no owner", None, false),
    ] {
        let event = Event {
            subject_owner: owner,
            ..event(T1, A, S)
        };
        assert_eq!(compiled.admits(&event), expected, "{label}");
    }
}

#[test]
fn an_owner_value_that_is_not_a_uuid_drops_its_constraint() {
    let scope = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(pep_properties::OWNER_ID, text("not-a-uuid")),
    ]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        not_enforceable(vec![DropReason::InvalidValue])
    );
}

#[test]
fn an_owner_filter_restricts_no_event_type() {
    let compiled = compile(
        &scope(vec![vec![ScopeFilter::eq(pep_properties::OWNER_ID, U1)]]),
        &TenantMode::RootOnly,
    )
    .unwrap();

    assert!(compiled.admits_event_type(&type_id(A)));
    assert_eq!(
        compiled.plan_for(&type_id(A)),
        TypePlan::Admit(Arc::from([CompiledConstraint {
            tenant: TenantCheck::Any,
            rest: Box::new([EventPredicate::OwnerIn(HashSet::from([U1]))]),
        }]))
    );
}

#[test]
fn a_filter_on_an_unknown_property_drops_its_constraint() {
    let subject = scope(vec![vec![ScopeFilter::eq("subject", text("x"))]]);
    assert_eq!(
        compile(&subject, &TenantMode::RootOnly),
        not_enforceable(vec![DropReason::UnknownProperty]),
        "subject"
    );

    let topic = scope(vec![vec![ScopeFilter::eq(
        "topic_id",
        text("gts.cf.core.events.topic.v1~x.eb.orders.acme.v1"),
    )]]);
    assert_eq!(
        compile(&topic, &TenantMode::RootOnly),
        not_enforceable(vec![DropReason::UnknownProperty]),
        "topic_id"
    );
}

#[test]
fn every_advertised_consume_property_names_a_field_the_scope_checks() {
    assert_eq!(
        CONSUME_PROPS.map(ScopeField::of),
        [
            Ok(ScopeField::Tenant),
            Ok(ScopeField::Owner),
            Ok(ScopeField::Type),
            Ok(ScopeField::SubjectType),
        ]
    );
}

#[test]
fn a_tenant_value_that_is_not_a_uuid_drops_its_constraint() {
    let string = scope(vec![vec![ScopeFilter::eq(TENANT, text("not-a-uuid"))]]);
    assert_eq!(
        compile(&string, &TenantMode::RootOnly),
        not_enforceable(vec![DropReason::InvalidValue]),
        "a string that is not a uuid"
    );

    let int = scope(vec![vec![ScopeFilter::eq(TENANT, ScopeValue::Int(7))]]);
    assert_eq!(
        compile(&int, &TenantMode::RootOnly),
        not_enforceable(vec![DropReason::InvalidValue]),
        "an int"
    );
}

#[test]
fn a_type_value_that_is_not_an_exact_type_id_drops_its_constraint() {
    let cases = [
        (EVENT_TYPE, ScopeValue::Uuid(T1), "event_type: a uuid"),
        (
            EVENT_TYPE,
            text("gts.cf.core.events.event.v1~x.eb.*"),
            "event_type: a wildcard pattern",
        ),
        (
            EVENT_TYPE,
            text("gts.cf.core.events.topic.v1~x.eb.orders.acme.v1"),
            "event_type: an instance id",
        ),
        (
            EVENT_TYPE,
            text("not a gts id"),
            "event_type: a malformed string",
        ),
        (
            EVENT_SUBJECT_TYPE,
            text("gts.cf.core.events.topic.v1~x.eb.orders.acme.v1"),
            "event_subject_type: an instance id",
        ),
    ];

    for (property, value, label) in cases {
        let scope = scope(vec![vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::eq(property, value),
        ]]);
        assert_eq!(
            compile(&scope, &TenantMode::RootOnly),
            not_enforceable(vec![DropReason::InvalidValue]),
            "{label}"
        );
    }
}

#[test]
fn a_subtree_or_group_filter_drops_its_constraint() {
    let cases = [
        (
            ScopeFilter::in_tenant_subtree(TENANT, T1, true, vec![]),
            "in_tenant_subtree",
        ),
        (
            ScopeFilter::in_group_typed(
                TENANT,
                "gts.x.eb.groups.member.v1~",
                vec![ScopeValue::Uuid(T1)],
            ),
            "in_group",
        ),
        (
            ScopeFilter::in_group_subtree_typed(
                TENANT,
                "gts.x.eb.groups.member.v1~",
                vec![ScopeValue::Uuid(T1)],
            ),
            "in_group_subtree",
        ),
    ];

    for (filter, label) in cases {
        let scope = scope(vec![vec![filter]]);
        assert_eq!(
            compile(&scope, &TenantMode::RootOnly),
            not_enforceable(vec![DropReason::UnsupportedFilter]),
            "{label}"
        );
    }
}

#[test]
fn the_first_failing_filter_names_the_reason() {
    let scope = scope(vec![vec![
        ScopeFilter::eq("subject", text("x")),
        ScopeFilter::eq(TENANT, text("bad")),
    ]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        not_enforceable(vec![DropReason::UnknownProperty])
    );
}

#[test]
fn a_dropped_constraint_narrows_the_scope_and_the_others_stay() {
    let scope = scope(vec![
        vec![ScopeFilter::eq("subject", text("x"))],
        vec![ScopeFilter::eq(TENANT, T1)],
        vec![ScopeFilter::eq(TENANT, T2)],
    ]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![
                CompiledConstraint {
                    tenant: TenantCheck::One(T1),
                    rest: Box::new([]),
                },
                CompiledConstraint {
                    tenant: TenantCheck::One(T2),
                    rest: Box::new([]),
                },
            ],
        })
    );
}

#[test]
fn a_scope_whose_every_constraint_is_dropped_lists_each_reason_in_order() {
    let scope = scope(vec![
        vec![ScopeFilter::eq("subject", text("x"))],
        vec![ScopeFilter::eq(TENANT, text("not-a-uuid"))],
        vec![ScopeFilter::eq(EVENT_TYPE, text(A))],
    ]);

    assert_eq!(
        compile(&scope, &TenantMode::Subtree),
        not_enforceable(vec![
            DropReason::UnknownProperty,
            DropReason::InvalidValue,
            DropReason::NoTenantFilter,
        ])
    );
}

#[test]
fn an_allow_all_scope_reads_any_tenant_under_root_only() {
    assert_eq!(
        compile(&AccessScope::allow_all(), &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::Any,
                rest: Box::new([]),
            }],
        })
    );
}

#[test]
fn an_allow_all_scope_is_not_enforceable_under_subtree() {
    assert_eq!(
        compile(&AccessScope::allow_all(), &TenantMode::Subtree),
        not_enforceable(vec![DropReason::NoTenantFilter])
    );
}

#[test]
fn a_deny_all_scope_is_not_enforceable() {
    assert_eq!(
        compile(&AccessScope::deny_all(), &TenantMode::RootOnly),
        not_enforceable(vec![]),
        "root only"
    );
    assert_eq!(
        compile(&AccessScope::deny_all(), &TenantMode::Subtree),
        not_enforceable(vec![]),
        "subtree"
    );
}

#[test]
fn the_error_text_names_no_value() {
    assert_eq!(
        ScopeNotEnforceable {
            dropped: vec![DropReason::InvalidValue]
        }
        .to_string(),
        "no constraint of the consume scope can be enforced"
    );
}

#[test]
fn admits_decides_each_event_by_the_kept_constraints() {
    let one = scope(vec![vec![ScopeFilter::eq(TENANT, T1)]]);
    let set = scope(vec![vec![ScopeFilter::in_uuids(TENANT, vec![T1, T2])]]);
    let any_of_type_a = scope(vec![vec![ScopeFilter::eq(EVENT_TYPE, text(A))]]);
    let one_of_subject_s = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S)),
    ]]);
    let one_of_type_a = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(EVENT_TYPE, text(A)),
    ]]);
    let one_of_type_a_subject_s = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(EVENT_TYPE, text(A)),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S)),
    ]]);
    let either = scope(vec![
        vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::eq(EVENT_TYPE, text(A)),
        ],
        vec![ScopeFilter::eq(TENANT, T2)],
    ]);

    let rows = [
        ("One(T1): T1", &one, event(T1, A, S), true),
        ("One(T1): T2", &one, event(T2, A, S), false),
        ("Set(T1, T2): T2", &set, event(T2, A, S), true),
        ("Set(T1, T2): T3", &set, event(T3, A, S), false),
        (
            "Any + TypeIn(A): type A",
            &any_of_type_a,
            event(T4, A, S),
            true,
        ),
        (
            "Any + TypeIn(A): type B",
            &any_of_type_a,
            event(T4, B, S),
            false,
        ),
        (
            "One(T1) + SubjectTypeIn(S): S",
            &one_of_subject_s,
            event(T1, A, S),
            true,
        ),
        (
            "One(T1) + SubjectTypeIn(S): S2",
            &one_of_subject_s,
            event(T1, A, S2),
            false,
        ),
        (
            "AND: T1 with type B",
            &one_of_type_a,
            event(T1, B, S),
            false,
        ),
        (
            "AND: T1 with type A and subject type S2",
            &one_of_type_a_subject_s,
            event(T1, A, S2),
            false,
        ),
        ("OR: T2 with type B", &either, event(T2, B, S), true),
        ("OR: T3 with type A", &either, event(T3, A, S), false),
    ];

    for (label, scope, event, expected) in rows {
        let compiled = compile(scope, &TenantMode::RootOnly).unwrap();
        assert_eq!(compiled.admits(&event), expected, "{label}");
    }
}

#[test]
fn admits_event_type_asks_whether_some_kept_constraint_names_the_type() {
    let tenant_only = scope(vec![vec![ScopeFilter::eq(TENANT, T1)]]);
    let type_a = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(EVENT_TYPE, text(A)),
    ]]);
    let subject_s_only = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S)),
    ]]);
    let type_b_or_type_a = scope(vec![
        vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::eq(EVENT_TYPE, text(B)),
        ],
        vec![
            ScopeFilter::eq(TENANT, T2),
            ScopeFilter::eq(EVENT_TYPE, text(A)),
        ],
    ]);

    let rows = [
        ("no type filter: A", &tenant_only, A, true),
        ("One(T1) + TypeIn(A): A", &type_a, A, true),
        ("One(T1) + TypeIn(A): B", &type_a, B, false),
        ("SubjectTypeIn(S) only: A", &subject_s_only, A, true),
        ("TypeIn(B) OR TypeIn(A): A", &type_b_or_type_a, A, true),
        ("TypeIn(B) OR TypeIn(A): B", &type_b_or_type_a, B, true),
    ];

    for (label, scope, event_type, expected) in rows {
        let compiled = compile(scope, &TenantMode::RootOnly).unwrap();
        assert_eq!(
            compiled.admits_event_type(&type_id(event_type)),
            expected,
            "{label}"
        );
    }
}

#[test]
fn a_subject_type_predicate_keeps_each_id_once() {
    let scope = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::r#in(
            EVENT_SUBJECT_TYPE,
            vec![text(S_RUSH), text(S), text(S_DERIVED), text(S)],
        ),
    ]]);

    assert_eq!(
        compile(&scope, &TenantMode::RootOnly),
        Ok(CompiledScope {
            any_of: vec![CompiledConstraint {
                tenant: TenantCheck::One(T1),
                rest: Box::new([EventPredicate::SubjectTypeIn(types(&[
                    S, S_DERIVED, S_RUSH
                ]))]),
            }],
        })
    );
}

/// Per kept constraint: any subject type when it has no subject-type filter,
/// otherwise the types every filter names that an allowed pattern matches.
#[test]
fn admitted_subject_types_keeps_per_constraint_the_allowed_types_every_filter_names() {
    let scope = scope(vec![
        vec![ScopeFilter::eq(TENANT, T1)],
        vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::r#in(
                EVENT_SUBJECT_TYPE,
                vec![text(S_DERIVED), text(S_RUSH), text(S2)],
            ),
            ScopeFilter::r#in(EVENT_SUBJECT_TYPE, vec![text(S2), text(S_DERIVED)]),
        ],
        vec![
            ScopeFilter::eq(TENANT, T2),
            ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S2)),
        ],
    ]);
    let compiled = compile(&scope, &TenantMode::RootOnly).unwrap();

    assert_eq!(
        compiled.admitted_subject_types(&[&pattern(S)]),
        AdmittedSubjectTypes {
            any_of: vec![
                SubjectTypesOf::Any,
                SubjectTypesOf::Only(parsed(&[S_DERIVED])),
                SubjectTypesOf::Only(parsed(&[])),
            ],
        }
    );
}

/// A constraint's admitted types come in id order, whatever order its filters
/// list them in. Each compile builds its filters' hash sets with new random
/// keys, so an unsorted answer lists these six types in id order about once
/// in 720 compiles, and in all three compiles here about once in 370 million
/// runs.
#[test]
fn admitted_subject_types_are_listed_in_id_order() {
    const S_BULK: &str = "gts.x.eb.orders.subject.v1~x.eb.orders.bulk.v1~";
    const S_GIFT: &str = "gts.x.eb.orders.subject.v1~x.eb.orders.gift.v1~";
    let scope = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::r#in(
            EVENT_SUBJECT_TYPE,
            vec![
                text(S_RUSH),
                text(S_GIFT),
                text(S),
                text(S2),
                text(S_DERIVED),
                text(S_MINOR),
                text(S_BULK),
            ],
        ),
        ScopeFilter::r#in(
            EVENT_SUBJECT_TYPE,
            vec![
                text(S_DERIVED),
                text(S_BULK),
                text(S_MINOR),
                text(S_RUSH),
                text(S),
                text(S_GIFT),
            ],
        ),
    ]]);

    for round in 0..3 {
        let compiled = compile(&scope, &TenantMode::RootOnly).unwrap();
        assert_eq!(
            compiled.admitted_subject_types(&[&pattern(ANY_X)]),
            AdmittedSubjectTypes {
                any_of: vec![SubjectTypesOf::Only(parsed(&[
                    S_MINOR, S, S_BULK, S_GIFT, S_RUSH, S_DERIVED
                ]))],
            },
            "compile {round}"
        );
    }
}

/// A label, the scope, the allowed patterns, the selector and the answer.
type AdmitsUnderRow<'a> = (&'a str, &'a AccessScope, &'a [&'a str], &'a str, bool);

/// A constraint admits a selector when it has no subject-type filter, or
/// when some type is named by every subject-type filter of it, matches an
/// allowed pattern, and is matched by the selector as delivery matches: the
/// type, a derived type or a minor version.
#[test]
fn admits_under_asks_whether_some_kept_constraint_admits_an_allowed_type_under_the_selector() {
    let tenant_only = scope(vec![vec![ScopeFilter::eq(TENANT, T1)]]);
    let subject_s = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S)),
    ]]);
    let type_a_only = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(EVENT_TYPE, text(A)),
    ]]);
    let subject_s2_or_subject_s = scope(vec![
        vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S2)),
        ],
        vec![
            ScopeFilter::eq(TENANT, T2),
            ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S)),
        ],
    ]);
    let derived_only = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S_DERIVED)),
    ]]);
    let minor_only = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S_MINOR)),
    ]]);
    let subject_s2_or_derived = scope(vec![
        vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S2)),
        ],
        vec![
            ScopeFilter::eq(TENANT, T2),
            ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S_DERIVED)),
        ],
    ]);
    let derived_and_rush = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S_DERIVED)),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S_RUSH)),
    ]]);
    let derived_in_both = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::r#in(EVENT_SUBJECT_TYPE, vec![text(S_DERIVED), text(S_RUSH)]),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S_DERIVED)),
    ]]);
    let s2_in_both = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::r#in(EVENT_SUBJECT_TYPE, vec![text(S2), text(S_RUSH)]),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S2)),
    ]]);

    let rows: [AdmitsUnderRow; 18] = [
        ("no subject type filter: S", &tenant_only, &[ANY_X], S, true),
        (
            "unrestricted, nothing allowed: S",
            &tenant_only,
            &[],
            S,
            true,
        ),
        (
            "One(T1) + SubjectTypeIn(S): S",
            &subject_s,
            &[ANY_X],
            S,
            true,
        ),
        (
            "One(T1) + SubjectTypeIn(S): S2",
            &subject_s,
            &[ANY_X],
            S2,
            false,
        ),
        (
            "One(T1) + SubjectTypeIn(S): S_DERIVED, not covered by its base",
            &subject_s,
            &[ANY_X],
            S_DERIVED,
            false,
        ),
        ("TypeIn(A) only: S", &type_a_only, &[ANY_X], S, true),
        (
            "SubjectTypeIn(S2) OR SubjectTypeIn(S): S",
            &subject_s2_or_subject_s,
            &[ANY_X],
            S,
            true,
        ),
        (
            "SubjectTypeIn(S2) OR SubjectTypeIn(S): S2",
            &subject_s2_or_subject_s,
            &[ANY_X],
            S2,
            true,
        ),
        (
            "SubjectTypeIn(S_DERIVED): S covers it",
            &derived_only,
            &[ANY_X],
            S,
            true,
        ),
        (
            "SubjectTypeIn(S_DERIVED): S2",
            &derived_only,
            &[ANY_X],
            S2,
            false,
        ),
        (
            "SubjectTypeIn(S_MINOR): S covers it",
            &minor_only,
            &[ANY_X],
            S,
            true,
        ),
        (
            "SubjectTypeIn(S2) OR SubjectTypeIn(S_DERIVED): S",
            &subject_s2_or_derived,
            &[ANY_X],
            S,
            true,
        ),
        (
            "no candidate-allowed id: SubjectTypeIn(S), allowed S2",
            &subject_s,
            &[S2],
            S,
            false,
        ),
        (
            "an id in only one of two sets: S_DERIVED AND S_RUSH",
            &derived_and_rush,
            &[ANY_X],
            S,
            false,
        ),
        (
            "an id in both sets, allowed and under S: S_DERIVED",
            &derived_in_both,
            &[S],
            S,
            true,
        ),
        (
            "an id in both sets and allowed, not under S: S2",
            &s2_in_both,
            &[ANY_X],
            S,
            false,
        ),
        (
            "OR across constraints, only the second allowed: S",
            &subject_s2_or_derived,
            &[S],
            S,
            true,
        ),
        (
            "OR across constraints, neither allowed: S",
            &subject_s2_or_derived,
            &[S_RUSH],
            S,
            false,
        ),
    ];

    for (label, scope, allowed, selector, expected) in rows {
        let compiled = compile(scope, &TenantMode::RootOnly).unwrap();
        let allowed: Vec<GtsIdPattern> = allowed.iter().map(|raw| pattern(raw)).collect();
        let allowed: Vec<&GtsIdPattern> = allowed.iter().collect();
        assert_eq!(
            compiled
                .admitted_subject_types(&allowed)
                .admits_under(&pattern(selector)),
            expected,
            "{label}"
        );
    }
}

/// Delivery needs the event's subject type in every subject-type filter of a
/// constraint.
#[test]
fn an_event_passes_two_subject_type_filters_only_when_both_name_its_subject_type() {
    let both = scope(vec![vec![
        ScopeFilter::eq(TENANT, T1),
        ScopeFilter::r#in(EVENT_SUBJECT_TYPE, vec![text(S_DERIVED), text(S_RUSH)]),
        ScopeFilter::eq(EVENT_SUBJECT_TYPE, text(S_DERIVED)),
    ]]);
    let compiled = compile(&both, &TenantMode::RootOnly).unwrap();

    assert_eq!(
        (
            compiled.admits(&event(T1, A, S_DERIVED)),
            compiled.admits(&event(T1, A, S_RUSH)),
        ),
        (true, false),
        "(in both sets, in one set)"
    );
}

#[test]
fn the_same_tenant_set_in_another_order_is_shared() {
    let interner = TenantSetInterner::new();
    let mut forward = HashSet::new();
    for t in [T1, T2, T3] {
        forward.insert(t);
    }
    let mut backward = HashSet::new();
    for t in [T3, T2, T1] {
        backward.insert(t);
    }

    let first = interner.intern(forward);
    let second = interner.intern(backward);

    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn two_compiles_of_the_same_set_share_one_allocation() {
    let interner = TenantSetInterner::new();
    let forward = scope(vec![vec![ScopeFilter::in_uuids(TENANT, vec![T1, T2, T3])]]);
    let backward = scope(vec![vec![ScopeFilter::in_uuids(TENANT, vec![T3, T1, T2])]]);

    let first = CompiledScope::compile(&forward, &TenantMode::RootOnly, &interner).unwrap();
    let second = CompiledScope::compile(&backward, &TenantMode::RootOnly, &interner).unwrap();

    let ([first], [second]) = (first.any_of.as_slice(), second.any_of.as_slice()) else {
        panic!("one constraint each: {first:?} {second:?}");
    };
    let (TenantCheck::Set(first), TenantCheck::Set(second)) = (&first.tenant, &second.tenant)
    else {
        panic!("tenant sets: {first:?} {second:?}");
    };
    assert_eq!(**first, HashSet::from([T1, T2, T3]));
    assert!(Arc::ptr_eq(first, second));
}

#[test]
fn different_sets_under_one_key_stay_apart() {
    let interner = TenantSetInterner::new();

    let low = interner.intern_keyed(7, HashSet::from([T1, T2]));
    let high = interner.intern_keyed(7, HashSet::from([T3, T4]));

    assert!(!Arc::ptr_eq(&low, &high));
    assert_eq!(*low, HashSet::from([T1, T2]));
    assert_eq!(*high, HashSet::from([T3, T4]));
    assert!(Arc::ptr_eq(
        &low,
        &interner.intern_keyed(7, HashSet::from([T1, T2]))
    ));
    assert!(Arc::ptr_eq(
        &high,
        &interner.intern_keyed(7, HashSet::from([T3, T4]))
    ));
}

#[test]
fn a_set_nobody_holds_is_pruned_on_the_next_intern() {
    let interner = TenantSetInterner::new();

    drop(interner.intern(HashSet::from([T1, T2])));
    let held = interner.intern(HashSet::from([T3, T4]));

    {
        let sets = interner.sets.lock().unwrap();
        let buckets: Vec<_> = sets.values().collect();
        let [bucket] = buckets.as_slice() else {
            panic!("one bucket: {sets:?}");
        };
        let [entry] = bucket.as_slice() else {
            panic!("one entry: {bucket:?}");
        };
        assert!(Arc::ptr_eq(&entry.upgrade().unwrap(), &held));
    }

    let again = interner.intern(HashSet::from([T1, T2]));
    assert_eq!(*again, HashSet::from([T1, T2]));
}

#[test]
fn a_poisoned_interner_still_interns() {
    let interner = Arc::new(TenantSetInterner::new());
    let poisoner = Arc::clone(&interner);
    let joined = thread::spawn(move || {
        let _guard = poisoner.sets.lock().unwrap();
        panic!("poison the interner lock");
    })
    .join();
    assert!(joined.is_err());
    assert!(interner.sets.is_poisoned());

    let first = interner.intern(HashSet::from([T1, T2]));
    let second = interner.intern(HashSet::from([T2, T1]));

    assert_eq!(*first, HashSet::from([T1, T2]));
    assert!(Arc::ptr_eq(&first, &second));
}

/// One constraint per row; the plan for `A` keeps the first three.
#[test]
fn a_plan_keeps_the_constraints_whose_type_filters_all_name_the_type() {
    let kept_rows = vec![
        // No type filter.
        vec![ScopeFilter::eq(TENANT, T1)],
        // A type filter naming `A`.
        vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::r#in(EVENT_TYPE, vec![text(A), text(B)]),
        ],
        // A subject-type filter only.
        vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::r#in(EVENT_SUBJECT_TYPE, vec![text(S)]),
        ],
    ];
    let dropped_rows = vec![
        // A type filter without `A`.
        vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::r#in(EVENT_TYPE, vec![text(B)]),
        ],
        // Two type filters, one without `A`.
        vec![
            ScopeFilter::eq(TENANT, T1),
            ScopeFilter::r#in(EVENT_TYPE, vec![text(A), text(B)]),
            ScopeFilter::r#in(EVENT_TYPE, vec![text(B)]),
        ],
    ];
    let all_rows = kept_rows.iter().chain(&dropped_rows).cloned().collect();
    let compiled = compile(&scope(all_rows), &TenantMode::Subtree).unwrap();
    let expected = compile(&scope(kept_rows), &TenantMode::Subtree).unwrap();

    assert_eq!(
        compiled.plan_for(&type_id(A)),
        TypePlan::Admit(expected.any_of.into())
    );
}

#[test]
fn a_plan_with_no_constraint_naming_the_type_denies_it() {
    let compiled = compile(
        &scope(vec![
            vec![
                ScopeFilter::eq(TENANT, T1),
                ScopeFilter::r#in(EVENT_TYPE, vec![text(B)]),
            ],
            vec![
                ScopeFilter::eq(TENANT, T2),
                ScopeFilter::r#in(EVENT_TYPE, vec![text(A)]),
                ScopeFilter::r#in(EVENT_TYPE, vec![text(B)]),
            ],
        ]),
        &TenantMode::Subtree,
    )
    .unwrap();

    assert_eq!(compiled.plan_for(&type_id(A)), TypePlan::Deny);
}

/// The plan decides by every kept constraint: the tenant, then the subject
/// type.
#[test]
fn a_plan_admits_an_event_one_of_its_constraints_admits() {
    let compiled = compile(
        &scope(vec![
            vec![ScopeFilter::eq(TENANT, T1)],
            vec![
                ScopeFilter::eq(TENANT, T2),
                ScopeFilter::r#in(EVENT_SUBJECT_TYPE, vec![text(S)]),
            ],
        ]),
        &TenantMode::Subtree,
    )
    .unwrap();
    let plan = compiled.plan_for(&type_id(A));

    assert!(plan.admits(&event(T1, A, S2)));
    assert!(plan.admits(&event(T2, A, S)));
    assert!(!plan.admits(&event(T2, A, S2)));
    assert!(!plan.admits(&event(T3, A, S)));
    assert!(!TypePlan::Deny.admits(&event(T1, A, S)));
}

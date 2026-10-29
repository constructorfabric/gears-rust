#![allow(clippy::unwrap_used, clippy::expect_used)]

use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    Checkpoint, SourceDefinition, SourceId, SourceState, TenantScope, plan_sources,
    service_identity,
};
use crate::domain::digest::canonical_json_text;
use crate::domain::usage::feed::{FeedSubscription, MeterId};

const T1: Uuid = Uuid::from_u128(0x10);
const T2: Uuid = Uuid::from_u128(0x20);
const T3: Uuid = Uuid::from_u128(0x30);

fn meter(name: &str) -> MeterId {
    MeterId::parse(format!("gts.cf.core.uc.usage_record.v2~cf.test.{name}.v1~")).unwrap()
}

fn source(meters: &[&str], roots: &[Uuid]) -> SourceDefinition {
    SourceDefinition::new(
        FeedSubscription::new(meters.iter().map(|m| meter(m))).unwrap(),
        TenantScope::new(roots.iter().copied()).unwrap(),
    )
}

fn stored(definition: &SourceDefinition, state: SourceState) -> Checkpoint {
    Checkpoint {
        source_id: definition.source_id(),
        definition: definition.clone(),
        cursor: None,
        state,
        updated_at: OffsetDateTime::UNIX_EPOCH,
        config_generation: 0,
    }
}

#[test]
fn the_source_id_names_the_meters_and_the_configured_scope_in_any_order() {
    let id = source(&["a", "b"], &[T1, T2]).source_id();
    assert_eq!(id, source(&["b", "a"], &[T2, T1]).source_id());
    assert_ne!(
        id,
        source(&["a"], &[T1, T2]).source_id(),
        "the meter set is in the id"
    );
    assert_ne!(
        id,
        source(&["a", "b"], &[T1]).source_id(),
        "the tenant scope is in the id"
    );
    assert!(id.to_string().starts_with("usage:"));
    assert_eq!(SourceId::parse(&id.to_string()), Some(id));
}

#[test]
fn the_source_id_is_pinned() {
    // The id names a checkpoint, a leadership and losses: a change of the namespace, the
    // canonical JSON or the service identity re-reads every source from Oldest, so it must be
    // deliberate and show up here.
    let definition = source(&["a"], &[T1]);
    assert_eq!(
        service_identity(),
        "bss-rating.system:5c1d7e2a-4b3f-5a6e-8d9c-0b1a2f3e4d5c"
    );
    assert_eq!(
        canonical_json_text(&definition.to_json()),
        concat!(
            r#"{"gts_type_ids":["gts.cf.core.uc.usage_record.v2~cf.test.a.v1~"],"#,
            r#""service_identity":"bss-rating.system:5c1d7e2a-4b3f-5a6e-8d9c-0b1a2f3e4d5c","#,
            r#""tenant_scope":["00000000-0000-0000-0000-000000000010"]}"#
        )
    );
    assert_eq!(
        definition.source_id().to_string(),
        "usage:edbab1d8-7374-54a4-9ef5-00b08e7400dc"
    );
    assert_eq!(
        SourceDefinition::from_json(&definition.to_json()).unwrap(),
        definition
    );
}

#[test]
fn a_stored_definition_that_is_not_the_written_shape_is_refused() {
    let valid = source(&["a"], &[T1]).to_json();
    let mut cases = Vec::new();
    for field in ["gts_type_ids", "tenant_scope", "service_identity"] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(field);
        cases.push((format!("without {field}"), missing));
    }
    for (what, field, value) in [
        ("meters not a list", "gts_type_ids", serde_json::json!("a")),
        ("a meter not text", "gts_type_ids", serde_json::json!([7])),
        (
            "a malformed meter",
            "gts_type_ids",
            serde_json::json!(["gts.other~"]),
        ),
        ("no meter", "gts_type_ids", serde_json::json!([])),
        (
            "a root not a UUID",
            "tenant_scope",
            serde_json::json!(["T1"]),
        ),
        ("no root", "tenant_scope", serde_json::json!([])),
        (
            "a nil root",
            "tenant_scope",
            serde_json::json!([Uuid::nil()]),
        ),
        (
            "an empty identity",
            "service_identity",
            serde_json::json!(""),
        ),
        (
            "an identity not text",
            "service_identity",
            serde_json::json!(1),
        ),
    ] {
        let mut changed = valid.clone();
        changed[field] = value;
        cases.push((what.to_owned(), changed));
    }
    cases.push(("not an object".to_owned(), serde_json::json!([])));
    for (what, value) in cases {
        assert!(
            SourceDefinition::from_json(&value).is_err(),
            "{what}: {value}"
        );
    }
}

#[test]
fn a_stored_definition_keeps_its_own_service_identity() {
    let mut stored = source(&["a"], &[T1]).to_json();
    stored["service_identity"] = serde_json::json!("bss-rating.system:another");
    let definition = SourceDefinition::from_json(&stored).unwrap();
    assert_ne!(definition.source_id(), source(&["a"], &[T1]).source_id());
    assert_eq!(definition.to_json(), stored);
}

#[test]
fn a_tenant_scope_has_a_root_and_its_owner_is_the_first() {
    assert!(TenantScope::new([]).is_err());
    assert!(TenantScope::new([Uuid::nil()]).is_err());
    let scope = TenantScope::new([T2, T1, T2]).unwrap();
    assert_eq!(scope.roots(), &[T1, T2]);
    assert_eq!(scope.owner(), T1);
    assert!(scope.contains_all(&TenantScope::new([T2]).unwrap()));
    assert!(!scope.contains_all(&TenantScope::new([T3]).unwrap()));
}

#[test]
fn a_new_configuration_creates_its_sources() {
    let configured = vec![source(&["a"], &[T1])];
    let plan = plan_sources(&configured, &[]);
    assert_eq!(plan.create, configured);
    assert!(plan.widened.is_empty(), "a first source widens nothing");
}

#[test]
fn a_changed_meter_set_is_a_new_source_and_the_old_one_drains() {
    // V07: the old cursor is never used with the new filter.
    let old = source(&["a"], &[T1]);
    let new = source(&["a", "b"], &[T1]);
    let plan = plan_sources(
        std::slice::from_ref(&new),
        &[stored(&old, SourceState::Active)],
    );
    assert_eq!(plan.create, vec![new]);
    assert_eq!(plan.drain, vec![old.source_id()]);
    assert!(plan.retire.is_empty() && plan.widened.is_empty());
}

#[test]
fn a_widened_scope_is_a_new_source_with_a_loss_for_the_exposed_tenant() {
    // V66.
    let old = source(&["a"], &[T1]);
    let new = source(&["a"], &[T1, T2]);
    let plan = plan_sources(
        std::slice::from_ref(&new),
        &[stored(&old, SourceState::Active)],
    );
    assert_eq!(plan.create, vec![new.clone()]);
    assert_eq!(plan.drain, vec![old.source_id()]);
    assert_eq!(
        plan.widened,
        vec![(new.source_id(), TenantScope::new([T2]).unwrap())]
    );
}

#[test]
fn a_scope_narrowed_on_purpose_retires_the_old_source_without_draining() {
    let old = source(&["a"], &[T1, T2]);
    let new = source(&["a"], &[T1]);
    let plan = plan_sources(&[new], &[stored(&old, SourceState::Active)]);
    assert_eq!(plan.retire, vec![old.source_id()]);
    assert!(plan.drain.is_empty());
}

#[test]
fn a_configured_source_stored_draining_or_retired_is_read_again() {
    let definition = source(&["a"], &[T1]);
    for state in [SourceState::Draining, SourceState::Retired] {
        let plan = plan_sources(
            std::slice::from_ref(&definition),
            &[stored(&definition, state)],
        );
        assert_eq!(plan.activate, vec![definition.source_id()]);
        assert!(plan.create.is_empty());
    }
    let unchanged = plan_sources(
        std::slice::from_ref(&definition),
        &[stored(&definition, SourceState::Active)],
    );
    assert!(unchanged.is_empty());
}

#[test]
fn a_stored_source_keeps_the_service_identity_it_was_created_under() {
    // When the service identity changes (T-D-83), a stored source is still the source it was:
    // it keeps its id, and reconciliation drains it in favour of the new identity's source.
    let current = source(&["a"], &[T1]);
    let mut stored_json = current.to_json();
    stored_json["service_identity"] = serde_json::json!("bss-rating.system:previous");
    let previous = SourceDefinition::from_json(&stored_json).unwrap();
    assert_eq!(previous.to_json(), stored_json);
    assert_ne!(previous.source_id(), current.source_id());
    let plan = plan_sources(
        std::slice::from_ref(&current),
        &[stored(&previous, SourceState::Active)],
    );
    assert_eq!(plan.create, vec![current]);
    assert_eq!(plan.drain, vec![previous.source_id()]);
}

#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::domain::state_table::{Row, StateTable};
use bss_orders_lifecycle_sdk::catalog::OrderState as S;

pub fn facts(state: OrderState) -> AggregateFacts {
    let t0 = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
    AggregateFacts {
        order_id: Uuid::from_u128(1),
        state,
        state_entered_at: t0,
        current_version: 2,
        version_allocation_high_water: 3,
        draft_revision: 4,
        pre_hold_state: None,
        resume_count: 1,
        amendment_count: 1,
        fulfillment_control_generation: 0,
        fulfillment_control_pending: None,
        spawn_signal_at: None,
        authorization_failure_tolerated_at: None,
        compensation_evidence: None,
        resource_tenant_id: Uuid::from_u128(10),
        seller_tenant_id: Uuid::from_u128(20),
        payer_tenant_id: Uuid::from_u128(30),
        category: "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1".into(),
        contract_id: None,
    }
}
fn t() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_790_000_100).unwrap()
}
fn version() -> VersionContribution {
    VersionContribution {
        candidate: 3,
        payer_tenant_id: Uuid::from_u128(31),
        category: "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1".into(),
        contract_id: None,
        market_currency: Some("EUR".into()),
        market_region: Some("DE".into()),
        amendment_reason: None,
    }
}
fn contribution_for(row: u8) -> AggregateContribution {
    match declared_kind(RowId::of(row)) {
        ContributionKind::None => AggregateContribution::None,
        ContributionKind::DraftEdit => AggregateContribution::DraftEdit(DraftHeaderEdit::default()),
        ContributionKind::Version => AggregateContribution::Version(version()),
        ContributionKind::BeginFulfillment => {
            AggregateContribution::BeginFulfillment { tolerated: false }
        }
        ContributionKind::SpawnSignal => AggregateContribution::SpawnSignal,
        ContributionKind::CompensationEvidence => {
            AggregateContribution::CompensationEvidence(serde_json::json!({"x": 1}))
        }
        ContributionKind::ForcedFailure => {
            AggregateContribution::ForcedFailure(serde_json::json!({"forced": true}))
        }
    }
}

#[test]
fn field_classification_is_complete_and_selects_one_trigger() {
    let c = FieldClassification::registered().unwrap();
    for (scope, name) in AUTHORED_FIELDS {
        assert!(c.class(*scope, name).is_some(), "{name}");
    }
    assert_eq!(
        c.class(FieldScope::Order, "seller_tenant_id"),
        Some(FieldClass::CommercialFrozen)
    );
    assert_eq!(
        c.class(FieldScope::Line, "external_reference"),
        Some(FieldClass::Administrative)
    );
    let select = |fields: &[(FieldScope, &str)]| c.select(fields);
    assert_eq!(
        select(&[(FieldScope::Order, "display_label")]),
        Some(Selection {
            trigger: Trigger::AdministrativeEdit,
            mixed: false
        })
    );
    assert_eq!(
        select(&[(FieldScope::Order, "resource_tenant_id")]),
        Some(Selection {
            trigger: Trigger::DraftMutate,
            mixed: false
        })
    );
    assert_eq!(
        select(&[
            (FieldScope::Line, "currency"),
            (FieldScope::Line, "internal_notes")
        ]),
        Some(Selection {
            trigger: Trigger::DraftMutate,
            mixed: true
        })
    );
    assert_eq!(select(&[]), None);
    assert_eq!(select(&[(FieldScope::Order, "renewalTerms")]), None);
}

#[test]
fn missing_duplicate_or_unknown_field_classes_fail_startup() {
    let mut declared = FIELD_CLASSES.to_vec();
    declared.retain(|(_, n, _)| *n != "billing_cycle");
    assert_eq!(
        FieldClassification::compile(AUTHORED_FIELDS, &declared).unwrap_err(),
        FieldClassError::Unclassified(FieldScope::Line, "billing_cycle")
    );
    let mut declared = FIELD_CLASSES.to_vec();
    declared.push((FieldScope::Order, "category", FieldClass::Administrative));
    assert_eq!(
        FieldClassification::compile(AUTHORED_FIELDS, &declared).unwrap_err(),
        FieldClassError::Duplicate(FieldScope::Order, "category")
    );
    let mut declared = FIELD_CLASSES.to_vec();
    declared.push((FieldScope::Order, "renewalTerms", FieldClass::Commercial));
    assert_eq!(
        FieldClassification::compile(AUTHORED_FIELDS, &declared).unwrap_err(),
        FieldClassError::NotAuthored(FieldScope::Order, "renewalTerms")
    );
    let mut authored = AUTHORED_FIELDS.to_vec();
    authored.push((FieldScope::Order, "category"));
    assert_eq!(
        FieldClassification::compile(&authored, FIELD_CLASSES).unwrap_err(),
        FieldClassError::DuplicateField(FieldScope::Order, "category")
    );
}

/// Every existing-order expanded key: the planned effects follow steps 14-21 exactly.
#[test]
fn every_row_plans_its_state_counter_and_field_effects() {
    let table = StateTable::registered().unwrap();
    for row in table.rows().iter().filter(|r| r.id.number() != 1) {
        for from in row.from.iter().flatten() {
            let mut f = facts(*from);
            if *from == S::OnHold {
                f.pre_hold_state = Some(S::Approved);
            }
            let effects = plan(row, &f, &contribution_for(row.id.number())).unwrap();
            let target = row.effective_target(*from, f.pre_hold_state).unwrap();
            assert_eq!(effects.target, target, "{}", row.id);
            assert_eq!(effects.state_changed, target != *from, "{}", row.id);
            check_row_effects(row, *from, &f, &effects, target);
        }
    }
}

/// Steps 18-21 of one planned row: dwell clock, pre-hold, counters, version, spawn, evidence.
fn check_row_effects(
    row: &Row,
    from: OrderState,
    f: &AggregateFacts,
    effects: &Effects,
    target: OrderState,
) {
    let after = apply(f, effects, t());
    // State-entry time moves only on a state change.
    assert_eq!(
        after.state_entered_at == t(),
        target != from,
        "{} from {from:?}",
        row.id
    );
    // Pre-hold: stored on entry to hold, cleared on leaving, kept on self-loops.
    match (target == S::OnHold, target != from) {
        (true, true) => assert_eq!(after.pre_hold_state, Some(from)),
        (false, true) => assert_eq!(after.pre_hold_state, None),
        (_, false) => assert_eq!(after.pre_hold_state, f.pre_hold_state),
    }
    // Monotonic counters.
    assert_eq!(
        after.resume_count,
        f.resume_count + i32::from(row.trigger == Trigger::Resume)
    );
    assert_eq!(
        after.amendment_count,
        f.amendment_count + i32::from(row.trigger == Trigger::Amendment)
    );
    assert_eq!(
        after.draft_revision,
        f.draft_revision + i64::from(row.id.number() == 2)
    );
    assert_eq!(
        after.current_version,
        if row.is_versioning() { 3 } else { 2 },
        "{}",
        row.id
    );
    assert_eq!(
        effects.version,
        row.is_versioning().then_some((3, 2)),
        "{}",
        row.id
    );
    assert_eq!(after.spawn_signal_at.is_some(), row.id.number() == 12);
    assert_eq!(
        after.compensation_evidence.is_some(),
        matches!(row.id.number(), 14 | 16 | 26 | 27 | 28 | 29)
    );
    assert_eq!(
        effects.generation_increment,
        row.id.number() == 12,
        "{}",
        row.id
    );
}

#[test]
fn contributions_the_row_does_not_declare_abort() {
    let table = StateTable::registered().unwrap();
    let row = |n| table.row(RowId::of(n)).unwrap();
    let draft = facts(S::Draft);
    assert_eq!(
        plan(row(2), &draft, &AggregateContribution::None).unwrap_err(),
        ContributionError::RowMismatch(RowId::of(2))
    );
    assert_eq!(
        plan(
            row(5),
            &draft,
            &AggregateContribution::DraftEdit(DraftHeaderEdit::default())
        )
        .unwrap_err(),
        ContributionError::RowMismatch(RowId::of(5))
    );
    assert_eq!(
        plan(row(1), &draft, &AggregateContribution::None).unwrap_err(),
        ContributionError::RowMismatch(RowId::of(1))
    );
    let mut v = version();
    v.candidate = 2;
    assert_eq!(
        plan(row(4), &draft, &AggregateContribution::Version(v.clone())).unwrap_err(),
        ContributionError::Candidate
    );
    v.candidate = 4;
    assert_eq!(
        plan(row(4), &draft, &AggregateContribution::Version(v)).unwrap_err(),
        ContributionError::Candidate
    );
    // Write-once spawn signal and no generation change while a control is pending.
    let mut fulfil = facts(S::InFulfillment);
    fulfil.spawn_signal_at = Some(t());
    assert_eq!(
        plan(row(12), &fulfil, &AggregateContribution::SpawnSignal).unwrap_err(),
        ContributionError::SpawnRewrite
    );
    let mut pending = facts(S::InFulfillment);
    pending.fulfillment_control_pending = Some(Uuid::from_u128(9));
    assert_eq!(
        plan(row(12), &pending, &AggregateContribution::SpawnSignal).unwrap_err(),
        ContributionError::ControlPending
    );
    // Resume without a stored target has no effective target (the engine refuses first).
    let held = facts(S::OnHold);
    assert_eq!(
        plan(row(22), &held, &AggregateContribution::None).unwrap_err(),
        ContributionError::NoTarget(RowId::of(22))
    );
}

#[test]
fn irreversible_facts_and_post_spawn_resume() {
    let table = StateTable::registered().unwrap();
    let row = |n| table.row(RowId::of(n)).unwrap();
    // Tolerance is set when admitted, preserved otherwise, never cleared.
    let mut approved = facts(S::Approved);
    let e = plan(
        row(11),
        &approved,
        &AggregateContribution::BeginFulfillment { tolerated: true },
    )
    .unwrap();
    assert_eq!(
        apply(&approved, &e, t()).authorization_failure_tolerated_at,
        Some(t())
    );
    approved.authorization_failure_tolerated_at = Some(t() - time::Duration::days(1));
    let e = plan(
        row(11),
        &approved,
        &AggregateContribution::BeginFulfillment { tolerated: false },
    )
    .unwrap();
    assert_eq!(
        apply(&approved, &e, t()).authorization_failure_tolerated_at,
        approved.authorization_failure_tolerated_at
    );
    // Post-spawn resume to fulfillment issues a successor generation; spawn stays recorded.
    let mut held = facts(S::OnHold);
    held.pre_hold_state = Some(S::InFulfillment);
    held.spawn_signal_at = Some(t() - time::Duration::hours(1));
    let e = plan(row(22), &held, &AggregateContribution::None).unwrap();
    assert!(e.generation_increment && e.issues_grant());
    let after = apply(&held, &e, t());
    assert_eq!(after.state, S::InFulfillment);
    assert_eq!(after.spawn_signal_at, held.spawn_signal_at);
    assert_eq!(after.fulfillment_control_generation, 1);
    assert_eq!(after.resume_count, 2);
    // A pre-spawn resume issues none.
    held.spawn_signal_at = None;
    assert!(
        !plan(row(22), &held, &AggregateContribution::None)
            .unwrap()
            .generation_increment
    );
}

#[test]
fn staged_subflows_are_declared_for_exactly_their_rows() {
    let table = StateTable::registered().unwrap();
    let row = |n| table.row(RowId::of(n)).unwrap();
    for n in [4, 18, 19, 20] {
        assert!(requires_commercial_attempt(row(n)));
    }
    assert!(!requires_commercial_attempt(row(1)));
    let mut fulfil = facts(S::InFulfillment);
    for n in [14, 15, 16, 21] {
        assert!(!requires_receiver_control(row(n), &fulfil), "pre-spawn {n}");
    }
    fulfil.spawn_signal_at = Some(t());
    for n in [14, 15, 16, 21] {
        assert!(requires_receiver_control(row(n), &fulfil), "post-spawn {n}");
    }
    // D-182 forced exit and completion are not staged controls.
    for n in [13, 28] {
        assert!(!requires_receiver_control(row(n), &fulfil));
    }
    let mut held = facts(S::OnHold);
    held.pre_hold_state = Some(S::InFulfillment);
    held.spawn_signal_at = Some(t());
    for n in [23, 26, 27] {
        assert!(requires_receiver_control(row(n), &held));
    }
    assert!(!requires_receiver_control(row(29), &held));
    held.pre_hold_state = Some(S::Approved);
    assert!(!requires_receiver_control(row(23), &held));
}

#[test]
fn administrative_changes_are_reconciled_with_the_locked_values() {
    use crate::domain::audit::{AdminAttribute as A, AdminChange, AdminField as F};
    let change = |field, prior: Option<&str>, new: Option<&str>| AdminChange {
        field,
        prior: prior.map(str::to_owned),
        new: new.map(str::to_owned),
    };
    let line = uuid::Uuid::from_u128(5);
    let named = [
        // Stale prior from the preparation snapshot: replaced by the locked value.
        change(F::Order(A::ExternalReference), Some("PO-1"), Some("PO-3")),
        // Already holds its new value at the lock: dropped.
        change(F::Order(A::DisplayLabel), Some("x"), None),
        // A line field with no stored row is NULL at the lock.
        change(F::Line(line, A::InternalNotes), None, Some("n")),
    ];
    let stored = |field| match field {
        F::Order(A::ExternalReference) => Some("PO-2".to_owned()),
        _ => None,
    };
    assert_eq!(
        reconcile_administrative(&named, stored),
        vec![
            change(F::Order(A::ExternalReference), Some("PO-2"), Some("PO-3")),
            change(F::Line(line, A::InternalNotes), None, Some("n")),
        ]
    );
    // Every named value already stored: nothing changes, which the change guard refuses.
    let unchanged = [change(
        F::Order(A::ExternalReference),
        Some("PO-1"),
        Some("PO-2"),
    )];
    assert!(reconcile_administrative(&unchanged, stored).is_empty());
}

#[test]
fn draft_header_edits_write_only_named_values_and_increment_the_revision_once() {
    let table = StateTable::registered().unwrap();
    let draft_mutate = table.lookup(S::Draft, Trigger::DraftMutate).unwrap();
    let draft = facts(S::Draft);
    let contract = Uuid::from_u128(77);
    let edit = DraftHeaderEdit {
        category: Some("gts.cf.bss.orders.category.v1~cf.bss.orders.change.v1".into()),
        contract_id: Some(Some(contract)),
    };
    let effects = plan(
        draft_mutate,
        &draft,
        &AggregateContribution::DraftEdit(edit.clone()),
    )
    .unwrap();
    assert!(effects.draft_revision_increment && effects.version.is_none());
    let mut after = apply(&draft, &effects, t());
    edit.apply_to(&mut after);
    assert_eq!(after.draft_revision, draft.draft_revision + 1);
    assert_eq!(after.current_version, draft.current_version);
    assert_eq!(after.state_entered_at, draft.state_entered_at);
    assert_eq!(after.contract_id, Some(contract));
    assert!(after.category.ends_with("change.v1"));
    // Omitted values stay as locked; an explicit clear removes the contract reference.
    let mut kept = after.clone();
    DraftHeaderEdit::default().apply_to(&mut kept);
    assert_eq!(kept, after);
    DraftHeaderEdit {
        category: None,
        contract_id: Some(None),
    }
    .apply_to(&mut kept);
    assert_eq!(kept.contract_id, None);
    assert_eq!(kept.category, after.category);
    // The axes never travel in the header edit; the seller is untouched.
    assert_eq!(kept.seller_tenant_id, draft.seller_tenant_id);
}

#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::domain::contributions::{FieldClassification, tests::facts};
use crate::domain::guards::GuardRegistry;
use crate::domain::state_table::{RowId, StateTable};
use crate::domain::transition::{
    Decision, DraftRevisions, EngineRefusal, Inputs, decide, decide_create,
};
use bss_orders_lifecycle_sdk::authoring::{Currency, SelectedItem};
use bss_orders_lifecycle_sdk::catalog::OrderState as S;
use time::OffsetDateTime;

fn fields() -> FieldClassification {
    FieldClassification::registered().unwrap()
}
fn member(id: u128, currency: &str) -> WorkingMember {
    WorkingMember {
        line_id: Uuid::from_u128(id),
        currency: currency.into(),
    }
}
fn base() -> DraftGuardFacts {
    DraftGuardFacts {
        mixed: false,
        names_seller: false,
        named_category: None,
        target_line: None,
        proposed_currency: None,
        inserts_line: false,
        working: vec![member(1, "EUR"), member(2, "EUR")],
        line_cap: 3,
    }
}
fn t() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_790_000_100).unwrap()
}
/// Run row 2's real decision procedure (registry order, admissibility, revisions) over facts.
fn decide_row2(guards: DraftGuardFacts, state: S, client: Option<i64>) -> Decision<'static> {
    let table: &'static StateTable = Box::leak(Box::new(StateTable::registered().unwrap()));
    let registry = GuardRegistry::registered(table).unwrap();
    let mut locked = facts(state);
    locked.current_version = 1;
    decide(
        table,
        &registry,
        Trigger::DraftMutate,
        &locked,
        1,
        DraftRevisions::Draft {
            client,
            prepared: locked.draft_revision,
        },
        &guards.bindings(),
        &Inputs::new(),
        t(),
    )
    .unwrap()
}
fn guard_reason(d: &Decision<'_>) -> Option<Reason> {
    match d {
        Decision::Guard { reason, .. } => Some(*reason),
        Decision::Engine(refusal) => Some(refusal.reason()),
        _ => None,
    }
}

#[test]
fn the_declared_row_two_guards_are_exactly_the_capture_bindings() {
    let table = StateTable::registered().unwrap();
    let registry = GuardRegistry::registered(&table).unwrap();
    let declared: Vec<_> = registry
        .row_guards(RowId::of(2))
        .iter()
        .map(|g| g.name)
        .collect();
    assert_eq!(
        declared,
        [
            MEMBERSHIP,
            MIXED,
            SELLER_FIXED,
            CATEGORY,
            CURRENCY,
            LINE_CAP
        ]
    );
    let create: Vec<_> = registry
        .row_guards(RowId::of(1))
        .iter()
        .map(|g| g.name)
        .collect();
    assert_eq!(create, [CATEGORY]);
}

#[test]
fn trigger_selection_uses_the_field_classes_alone() {
    let f = fields();
    let commercial = HeaderPatch {
        payer_tenant_id: Some(Uuid::from_u128(31)),
        ..HeaderPatch::default()
    };
    let s = select_header(&f, &commercial).unwrap();
    assert_eq!((s.trigger, s.mixed), (Trigger::DraftMutate, false));
    let frozen = HeaderPatch {
        seller_tenant_id: Some(Uuid::from_u128(20)),
        ..HeaderPatch::default()
    };
    assert_eq!(
        select_header(&f, &frozen).unwrap().trigger,
        Trigger::DraftMutate
    );
    let admin = HeaderPatch {
        display_label: Some(Some("x".into())),
        external_reference: Some(None),
        ..HeaderPatch::default()
    };
    let s = select_header(&f, &admin).unwrap();
    assert_eq!((s.trigger, s.mixed), (Trigger::AdministrativeEdit, false));
    assert!(!is_draft_mutation(s));
    let mixed = LinePatch {
        currency: Some(Currency::try_from("USD".to_owned()).unwrap()),
        internal_notes: Some(Some("n".into())),
        ..LinePatch::default()
    };
    let s = select_line(&f, &mixed).unwrap();
    assert_eq!((s.trigger, s.mixed), (Trigger::DraftMutate, true));
    // Nothing named selects nothing (boundary-invalid).
    assert_eq!(select_header(&f, &HeaderPatch::default()), None);
}

#[test]
fn every_patch_wire_field_is_classified_once_in_the_shared_declaration() {
    let f = fields();
    let header = serde_json::json!({
        "category": Category::NEW_SALE, "payer_tenant_id": Uuid::nil(), "contract_id": null,
        "resource_tenant_id": Uuid::nil(), "seller_tenant_id": Uuid::nil(),
        "external_reference": null, "display_label": null, "internal_notes": null,
    });
    let header: HeaderPatch = serde_json::from_value(header).unwrap();
    assert_eq!(header.named_fields().len(), 8);
    for name in header.named_fields() {
        assert!(f.class(FieldScope::Order, name).is_some(), "{name}");
    }
    let line = serde_json::json!({
        "plan_id": Uuid::nil(), "plan_revision_id": Uuid::nil(), "selected_items": [],
        "currency": "EUR", "contract_effective_date": null, "service_activation_date": null,
        "acceptance_due_date": null, "term_duration": null, "billing_cycle": null,
        "external_reference": null, "display_label": null, "internal_notes": null,
    });
    let line: LinePatch = serde_json::from_value(line).unwrap();
    assert_eq!(line.named_fields().len(), 12);
    for name in line.named_fields() {
        assert!(f.class(FieldScope::Line, name).is_some(), "{name}");
    }
    // The S1-02 machine catalog lists exactly these PATCH names.
    let models: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/implementation/contracts/models.json"
    ))
    .unwrap();
    for (scope, named) in [
        ("header", header.named_fields()),
        ("line", line.named_fields()),
    ] {
        let classes = &models["field_classes"][scope];
        let mut catalog: Vec<&str> = ["commercial", "commercial_frozen", "administrative"]
            .iter()
            .flat_map(|c| classes[*c].as_array().unwrap())
            .map(|v| v.as_str().unwrap())
            .collect();
        let mut named = named.clone();
        catalog.sort_unstable();
        named.sort_unstable();
        assert_eq!(catalog, named, "{scope}");
    }
}

#[test]
fn structural_guards_refuse_with_their_registered_reasons() {
    // Removed/never-inserted line.
    let mut f = base();
    f.target_line = Some(Uuid::from_u128(9));
    assert_eq!(
        guard_reason(&decide_row2(f, S::Draft, Some(4))),
        Some(Reason::LineNotFound)
    );
    // Mixed classes, seller named (even with its current value), change category.
    let mut f = base();
    f.mixed = true;
    assert_eq!(
        guard_reason(&decide_row2(f, S::Draft, Some(4))),
        Some(Reason::MixedFieldClasses)
    );
    let mut f = base();
    f.names_seller = true;
    assert_eq!(
        guard_reason(&decide_row2(f, S::Draft, Some(4))),
        Some(Reason::TenantAxisImmutable)
    );
    let mut f = base();
    f.named_category = Some(Category::Change);
    assert_eq!(
        guard_reason(&decide_row2(f, S::Draft, Some(4))),
        Some(Reason::CategoryNotAdmitted)
    );
    // Mixed currency on insert; an edit of the only other-currency line is consistent.
    let mut f = base();
    f.inserts_line = true;
    f.proposed_currency = Some("USD".into());
    assert_eq!(
        guard_reason(&decide_row2(f, S::Draft, Some(4))),
        Some(Reason::CurrencyMixed)
    );
    let mut f = base();
    f.working = vec![member(1, "EUR")];
    f.target_line = Some(Uuid::from_u128(1));
    f.proposed_currency = Some("USD".into());
    assert!(matches!(
        decide_row2(f, S::Draft, Some(4)),
        Decision::Admitted(_)
    ));
    // Over-cap insertion refuses; an edit or removal at the cap does not consume capacity.
    let mut f = base();
    f.working.push(member(3, "EUR"));
    f.inserts_line = true;
    f.proposed_currency = Some("EUR".into());
    assert_eq!(
        guard_reason(&decide_row2(f, S::Draft, Some(4))),
        Some(Reason::LineCapExceeded)
    );
    let mut f = base();
    f.working.push(member(3, "EUR"));
    f.target_line = Some(Uuid::from_u128(3));
    assert!(matches!(
        decide_row2(f, S::Draft, Some(4)),
        Decision::Admitted(_)
    ));
}

#[test]
fn guard_precedence_follows_registration_and_the_engine_checks_come_first() {
    // Every guard would fail: membership, the first registered, wins.
    let all = DraftGuardFacts {
        mixed: true,
        names_seller: true,
        named_category: Some(Category::Change),
        target_line: Some(Uuid::from_u128(9)),
        proposed_currency: Some("USD".into()),
        inserts_line: true,
        working: vec![member(1, "EUR"), member(2, "EUR"), member(3, "EUR")],
        line_cap: 3,
    };
    assert_eq!(
        guard_reason(&decide_row2(all.clone(), S::Draft, Some(4))),
        Some(Reason::LineNotFound)
    );
    let mut f = all.clone();
    f.target_line = None;
    assert_eq!(
        guard_reason(&decide_row2(f.clone(), S::Draft, Some(4))),
        Some(Reason::MixedFieldClasses)
    );
    f.mixed = false;
    assert_eq!(
        guard_reason(&decide_row2(f.clone(), S::Draft, Some(4))),
        Some(Reason::TenantAxisImmutable)
    );
    f.names_seller = false;
    assert_eq!(
        guard_reason(&decide_row2(f.clone(), S::Draft, Some(4))),
        Some(Reason::CategoryNotAdmitted)
    );
    f.named_category = None;
    assert_eq!(
        guard_reason(&decide_row2(f.clone(), S::Draft, Some(4))),
        Some(Reason::CurrencyMixed)
    );
    f.proposed_currency = Some("EUR".into());
    assert_eq!(
        guard_reason(&decide_row2(f, S::Draft, Some(4))),
        Some(Reason::LineCapExceeded)
    );
    // Outside draft, admissibility wins over every guard, even with the revision omitted.
    let decision = decide_row2(all.clone(), S::Submitted, None);
    assert!(matches!(
        decision,
        Decision::Engine(EngineRefusal::NotAdmissible {
            state: S::Submitted,
            trigger: Trigger::DraftMutate
        })
    ));
    // Inside draft, an omitted or stale revision conflicts before any guard.
    for client in [None, Some(3)] {
        assert!(matches!(
            decide_row2(all.clone(), S::Draft, client),
            Decision::Engine(EngineRefusal::VersionConflict {
                current_version: 1,
                draft_revision: Some(4)
            })
        ));
    }
}

#[test]
fn create_admits_only_new_sale() {
    let table = StateTable::registered().unwrap();
    let registry = GuardRegistry::registered(&table).unwrap();
    let mut proposed = facts(S::Draft);
    proposed.category = Category::CHANGE.into();
    let refusal = decide_create(
        &table,
        &registry,
        &proposed,
        &create_bindings(),
        &Inputs::new(),
        t(),
    )
    .unwrap();
    assert_eq!(refusal, Some((Reason::CategoryNotAdmitted, false)));
    proposed.category = Category::NEW_SALE.into();
    assert_eq!(
        decide_create(
            &table,
            &registry,
            &proposed,
            &create_bindings(),
            &Inputs::new(),
            t()
        )
        .unwrap(),
        None
    );
    // An unregistered stored value fails closed.
    assert_eq!(
        category_verdict(None, "gts.cf.bss.orders.category.v1~x.v1"),
        GuardVerdict::Fail(Reason::CategoryNotAdmitted)
    );
}

#[test]
fn term_columns_preserve_intent_and_calendar_units() {
    assert_eq!(
        term_columns(None, Some(BillingCycle::Month)),
        ("missing", serde_json::json!({"kind": "missing"}), None)
    );
    assert_eq!(
        term_columns(Some(&AuthoredTerm::Rolling), None),
        ("rolling", serde_json::json!({"kind": "rolling"}), None)
    );
    let periods = AuthoredTerm::Periods { count: 2 };
    assert_eq!(
        term_columns(Some(&periods), Some(BillingCycle::Year)),
        (
            "finite",
            serde_json::json!({"kind": "periods", "count": 2}),
            Some("24 mons 0 days 0 microseconds".into())
        )
    );
    // No cycle yet: the intent is kept with no interval.
    assert_eq!(term_columns(Some(&periods), None).2, None);
    let calendar = AuthoredTerm::Calendar {
        years: 1,
        months: 2,
        days: 3,
        microseconds: 4,
    };
    let (kind, json, interval) = term_columns(Some(&calendar), None);
    assert_eq!(kind, "finite");
    assert_eq!(
        json,
        serde_json::json!({"kind": "calendar", "years": 1, "months": 2, "days": 3, "microseconds": 4})
    );
    assert_eq!(interval.as_deref(), Some("14 mons 3 days 4 microseconds"));
    assert_eq!(stored_term("finite", &json).unwrap(), Some(calendar));
    assert_eq!(
        stored_term("missing", &serde_json::json!({"kind": "missing"})).unwrap(),
        None
    );
    assert!(stored_term("other", &json).is_err());
}

#[test]
fn authored_values_are_bounded_structurally() {
    // Zero/negative quantities and zero terms are boundary-invalid; different cycles are fine.
    for quantity in ["0", "-1", "-0.5"] {
        let item = serde_json::json!({"item_id": Uuid::nil(), "quantity": quantity});
        assert!(
            serde_json::from_value::<SelectedItem>(item).is_err(),
            "{quantity}"
        );
    }
    for quantity in ["1", "0.25", "12.500"] {
        let item = serde_json::json!({"item_id": Uuid::nil(), "quantity": quantity});
        assert!(
            serde_json::from_value::<SelectedItem>(item).is_ok(),
            "{quantity}"
        );
    }
    // A JSON number is not an exact decimal string.
    let float = serde_json::json!({"item_id": Uuid::nil(), "quantity": 1.5});
    assert!(serde_json::from_value::<SelectedItem>(float).is_err());
    assert!(AuthoredTerm::Periods { count: 0 }.validate().is_err());
    assert!(
        AuthoredTerm::Calendar {
            years: 0,
            months: 0,
            days: 0,
            microseconds: 0
        }
        .validate()
        .is_err()
    );
    assert!(AuthoredTerm::Periods { count: 12 }.validate().is_ok());
    assert!(Currency::try_from("eur".to_owned()).is_err());
    assert!(Currency::try_from("EURO".to_owned()).is_err());
}

#[test]
fn shared_structural_predicates_serve_submit_and_amendment_rows_too() {
    assert_eq!(currency_verdict(["EUR", "EUR"]), GuardVerdict::Pass);
    assert_eq!(currency_verdict(std::iter::empty()), GuardVerdict::Pass);
    assert_eq!(
        currency_verdict(["EUR", "USD"]),
        GuardVerdict::Fail(Reason::CurrencyMixed)
    );
    assert_eq!(line_cap_verdict(200, 200), GuardVerdict::Pass);
    assert_eq!(
        line_cap_verdict(201, 200),
        GuardVerdict::Fail(Reason::LineCapExceeded)
    );
    // The same registered guards are declared on submit and the amendment rows.
    let table = StateTable::registered().unwrap();
    let registry = GuardRegistry::registered(&table).unwrap();
    for row in [4, 18, 19, 20] {
        let names: Vec<_> = registry
            .row_guards(RowId::of(row))
            .iter()
            .map(|g| g.name)
            .collect();
        for shared in [CATEGORY, CURRENCY, LINE_CAP] {
            assert!(names.contains(&shared), "row {row} {shared}");
        }
    }
}

#[test]
fn startup_refuses_an_unclassified_authoring_wire_field() {
    use crate::domain::contributions::{AUTHORED_FIELDS, FIELD_CLASSES};
    use bss_orders_lifecycle_sdk::authoring::{AddLine, CreateOrder};
    // The names come from the derived serde impls, so they track the SDK bodies exactly.
    assert_eq!(wire_fields::<HeaderPatch>().len(), 8);
    assert_eq!(wire_fields::<LinePatch>().len(), 12);
    assert_eq!(wire_fields::<AddLine>().len(), 9);
    assert_eq!(wire_fields::<CreateOrder>().len(), 5);
    assert!(wire_fields::<String>().is_empty());
    check_wire_classification(&fields()).unwrap();
    // A shared declaration that drops one authored field fails the startup check by name.
    let authored: Vec<_> = AUTHORED_FIELDS
        .iter()
        .copied()
        .filter(|(_, n)| *n != "billing_cycle")
        .collect();
    let declared: Vec<_> = FIELD_CLASSES
        .iter()
        .copied()
        .filter(|(_, n, _)| *n != "billing_cycle")
        .collect();
    let reduced = FieldClassification::compile(&authored, &declared).unwrap();
    let error = check_wire_classification(&reduced).unwrap_err();
    assert!(error.contains("billing_cycle"), "{error}");
    // The engine's startup compilation runs the check.
    crate::infra::engine::Registries::compile().unwrap();
}

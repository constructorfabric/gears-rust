//! Compile and run the prototype assertions before the domain exists.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use bss_pricing::domain::{
    plan::{ReferenceState as PlanReferenceState, RevisionState, Treatment},
    price::{ChangeKind, Eligibility, PriceState},
    price_book_entry::{ChargeKind, Model, OpState, ReferenceState},
    reference_op::{OpKind, RefKind},
};
/// Each phase 3 CHECK is spelled once per dialect: both halves carry the enum's exact vocabulary.
macro_rules! pin_both_dialects {
    ($ty:ty,$ddl:expr,$column:literal) => {{
        let values = <$ty>::ALL
            .iter()
            .map(|v| {
                let s = v.as_str();
                assert_eq!(s.parse::<$ty>().unwrap(), *v);
                format!("'{s}'")
            })
            .collect::<Vec<_>>()
            .join(",");
        let check = format!("CHECK ({} IN ({}))", $column, values);
        assert_eq!($ddl.matches(&check).count(), 2, "{check}");
        assert!("invalid".parse::<$ty>().is_err());
    }};
}
#[test]
fn enum_check_values_match_migration_text() {
    let entry = include_str!(
        "../src/infra/storage/migrations/m20260926_000005_create_pricing_price_book_entry.rs"
    );
    let price =
        include_str!("../src/infra/storage/migrations/m20260926_000007_create_pricing_price.rs");
    let op = include_str!(
        "../src/infra/storage/migrations/m20260926_000006_create_pricing_reference_op.rs"
    );
    // Each migration spells its CHECK once for Postgres and once for SQLite: both halves must
    // carry the enum's vocabulary, not one of them (PT-13).
    pin_both_dialects!(ChargeKind, entry, "charge_kind");
    pin_both_dialects!(Eligibility, price, "eligibility");
    pin_both_dialects!(OpState, op, "state");
    // D-522 widened `reference_state` with `released`. 000005 keeps the original three.
    let archived = include_str!("../src/infra/storage/migrations/m20261003_000023_book_archive.rs");
    pin_both_dialects!(ReferenceState, archived, "reference_state");
    assert!(
        entry.contains("CHECK (reference_state IN ('confirmation_pending','confirmed','lost'))")
    );
    // D-520 widened `state` and added `change_kind`. 000007 keeps the original four states.
    // Both new CHECKs are spelled once for Postgres and once for SQLite.
    let widened =
        include_str!("../src/infra/storage/migrations/m20261003_000022_price_cancel_and_end.rs");
    pin_both_dialects!(PriceState, widened, "state");
    pin_both_dialects!(ChangeKind, widened, "change_kind");
}
/// The revision state's CHECK is `m20260929_000017`'s since D-446 widened it with `scheduled`:
/// Postgres re-adds it, and the `SQLite` family rebuild spells it in the new table.
#[test]
fn plan_enum_check_values_match_migration_text() {
    let revision =
        include_str!("../src/infra/storage/migrations/m20260929_000017_revision_scheduled.rs");
    let item = include_str!(
        "../src/infra/storage/migrations/m20260926_000012_create_pricing_plan_item.rs"
    );
    pin_both_dialects!(RevisionState, revision, "state");
    pin_both_dialects!(Treatment, item, "treatment");
    pin_both_dialects!(PlanReferenceState, item, "reference_state");
}
/// A reference op names its reference as `(ref_kind, ref_id)` and its kind without the entry
/// suffix (D-412): both vocabularies are the enums', on both dialects.
#[test]
fn reference_op_kind_and_ref_kind_match_migration_text() {
    let op = include_str!(
        "../src/infra/storage/migrations/m20260926_000006_create_pricing_reference_op.rs"
    );
    // D-522 widened `kind` with `release`; 000006 keeps the original four.
    let archived = include_str!("../src/infra/storage/migrations/m20261003_000023_book_archive.rs");
    pin_both_dialects!(OpKind, archived, "kind");
    assert!(op.contains("CHECK (kind IN ('create','delete','rereserve','attach'))"));
    pin_both_dialects!(RefKind, op, "ref_kind");
    assert_eq!(
        OpKind::ALL.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        ["create", "delete", "rereserve", "attach", "release"]
    );
    assert_eq!(
        RefKind::ALL.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        ["price_book_entry", "plan_item"]
    );
}
/// D-427: the model moved from the price to the entry. Its CHECK is spelled once per dialect in
/// `m20260926_000013` (the price's own column, and with it 000007's CHECK, is dropped there), with
/// the enum's exact vocabulary.
#[test]
fn the_entry_model_check_matches_the_enum_on_000013() {
    let model =
        include_str!("../src/infra/storage/migrations/m20260926_000013_model_on_the_entry.rs");
    pin_both_dialects!(Model, model, "model");
}

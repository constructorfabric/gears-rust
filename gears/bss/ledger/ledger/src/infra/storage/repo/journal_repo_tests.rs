//! Unit tests for the pure `$orderby` helpers in `journal_repo.rs`.
//!
//! Both are pure rewrites of an [`ODataQuery`]'s order, so the whole contract
//! is testable without a database. The keyset walks they feed are exercised
//! end-to-end by the Postgres tier; what is checked here is the rewrite
//! itself, including the idempotence the cursor path depends on.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, SortDir};

use super::{query_with_default_order, query_with_unique_order};
use crate::odata::BalanceFilterField;

/// A caller `$orderby`, as the extractor would have parsed it.
fn ordered_by(field: &str) -> ODataQuery {
    ODataQuery::new().with_order(ODataOrderBy(vec![OrderKey {
        field: field.to_owned(),
        dir: SortDir::Asc,
    }]))
}

// ---------------------------------------------------------------------------
// query_with_default_order
// ---------------------------------------------------------------------------

#[test]
fn a_bare_list_gets_the_default_keyset_order() {
    let out = query_with_default_order(&ODataQuery::new(), "account_id");
    assert!(out.order.equals_signed_tokens("+account_id"));
}

#[test]
fn a_callers_orderby_is_left_alone() {
    let out = query_with_default_order(&ordered_by("account_class"), "account_id");
    assert!(out.order.equals_signed_tokens("+account_class"));
}

// ---------------------------------------------------------------------------
// query_with_unique_order
// ---------------------------------------------------------------------------

// The defect this exists for: `paginate_odata` appends exactly one tiebreaker,
// so a caller's `$orderby` would leave the balances walk ordered by
// `[account_class, account_id]` — and an account held in two currencies has two
// rows sharing that pair, one of which falls out at a page boundary. The
// `currency` suffix completes the `(tenant_id, account_id, currency)` key.
#[test]
fn the_suffix_completes_a_composite_key_behind_a_callers_orderby() {
    let out = query_with_unique_order(
        &ordered_by("account_class"),
        &[BalanceFilterField::Currency],
    );
    assert!(out.order.equals_signed_tokens("+account_class,+currency"));
}

#[test]
fn the_suffix_lands_behind_the_default_order_too() {
    let seeded = query_with_default_order(&ODataQuery::new(), "account_id");
    let out = query_with_unique_order(&seeded, &[BalanceFilterField::Currency]);
    assert!(out.order.equals_signed_tokens("+account_id,+currency"));
}

/// Page 2 rebuilds its order from `cursor.s`, which already carries the suffix,
/// and the helper runs again on that rebuilt query. A second application must
/// therefore be a no-op or every page would grow another `currency` key.
#[test]
fn applying_the_suffix_twice_changes_nothing() {
    let once = query_with_unique_order(
        &ordered_by("account_class"),
        &[BalanceFilterField::Currency],
    );
    let twice = query_with_unique_order(&once, &[BalanceFilterField::Currency]);
    // `ODataOrderBy` is not `PartialEq`; its `Display` is the readable form.
    assert_eq!(twice.order.to_string(), once.order.to_string());
    assert!(twice.order.equals_signed_tokens("+account_class,+currency"));
}

/// Same skip, reached the other way: a caller who already ordered by the
/// suffix field keeps their own direction rather than gaining a duplicate
/// ascending key behind it.
#[test]
fn a_suffix_field_the_caller_already_ordered_by_is_not_duplicated() {
    let descending = ODataQuery::new().with_order(ODataOrderBy(vec![OrderKey {
        field: "currency".to_owned(),
        dir: SortDir::Desc,
    }]));
    let out = query_with_unique_order(&descending, &[BalanceFilterField::Currency]);
    assert!(out.order.equals_signed_tokens("-currency"));
}

#[test]
fn an_empty_suffix_leaves_the_order_untouched() {
    let empty: [BalanceFilterField; 0] = [];
    let out = query_with_unique_order(&ordered_by("account_class"), &empty);
    assert!(out.order.equals_signed_tokens("+account_class"));
}

/// A multi-field suffix appends in the order given — the key halves of a
/// three-column primary key have to line up with the index, not arrive sorted.
#[test]
fn a_multi_field_suffix_keeps_the_order_it_was_given() {
    let out = query_with_unique_order(
        &ordered_by("account_class"),
        &[BalanceFilterField::AccountId, BalanceFilterField::Currency],
    );
    assert!(
        out.order
            .equals_signed_tokens("+account_class,+account_id,+currency")
    );
}

/// The suffix is spelled from the `FilterField` roster rather than a string
/// literal, so a renamed variant cannot silently order by a column the mapper
/// no longer recognises.
#[test]
fn the_suffix_field_names_come_from_the_filter_roster() {
    use toolkit_odata::filter::FilterField;
    assert_eq!(BalanceFilterField::Currency.name(), "currency");
    assert_eq!(BalanceFilterField::AccountId.name(), "account_id");
}

/// `$filter`, `limit` and the rest of the query ride through untouched — the
/// helper rewrites the order and nothing else.
#[test]
fn the_rest_of_the_query_rides_through() {
    let query = ordered_by("account_class").with_limit(7);
    let out = query_with_unique_order(&query, &[BalanceFilterField::Currency]);
    assert_eq!(out.limit, Some(7));
    assert!(out.cursor.is_none());
    assert!(out.filter.is_none());
}

/// Stored journal row with all independent metadata populated.
fn money_line(
    amount: &str,
    currency: &str,
    scale: i16,
    side: &str,
) -> crate::infra::storage::entity::journal_line::Model {
    use crate::infra::storage::entity::journal_line;
    use uuid::Uuid;
    journal_line::Model {
        line_id: Uuid::new_v4(),
        entry_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        period_id: "2026-10".to_owned(),
        payer_tenant_id: Uuid::new_v4(),
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::new_v4(),
        account_class: "UNALLOCATED".to_owned(),
        gl_code: None,
        side: side.to_owned(),
        amount: amount.to_owned(),
        currency: currency.to_owned(),
        currency_scale: scale,
        invoice_id: None,
        due_date: None,
        revenue_stream: None,
        mapping_status: "MAPPED".to_owned(),
        functional_amount: None,
        functional_currency: None,
        functional_currency_scale: None,
        tax_jurisdiction: None,
        tax_filing_period: None,
        tax_rate_ref: None,
        legal_entity_id: None,
        invoice_item_ref: None,
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
        po_allocation_group: None,
        credit_grant_event_type: None,
        ar_status: None,
        rate_snapshot_ref: None,
    }
}

#[test]
fn journal_projection_preserves_historical_and_functional_scale() {
    let mut row = money_line("1.001", "EUR", 3, "CR");
    row.functional_amount = Some("0".to_owned());
    row.functional_currency = Some("JPY".to_owned());
    row.functional_currency_scale = Some(0);
    let record = super::line_to_record(row.clone()).unwrap();
    assert_eq!(record.money.currency().scale(), 3);
    assert_eq!(record.functional_money.unwrap().currency().scale(), 0);
    row.functional_currency_scale = None;
    assert!(matches!(
        super::line_to_record(row),
        Err(crate::domain::model::RepoError::InvalidStoredMoney(_))
    ));
}

#[test]
fn settlement_total_is_currency_qualified_and_validates_scale() {
    let eur = bss_ledger_sdk::CurrencySpec::try_new("EUR".to_owned(), 2).unwrap();
    let lines = [
        money_line("5", "EUR", 2, "CR"),
        money_line("1", "EUR", 2, "DR"),
    ];
    assert_eq!(
        super::settled_total(&lines, &eur)
            .unwrap()
            .amount()
            .to_string(),
        "4"
    );
    let conflict = [money_line("1", "EUR", 3, "DR")];
    assert!(matches!(
        super::settled_total(&conflict, &eur),
        Err(crate::domain::model::RepoError::Money(
            bss_ledger_sdk::MoneyError::ScaleMismatch
        ))
    ));
    let wrong_currency = [money_line("1", "USD", 2, "CR")];
    assert!(matches!(
        super::settled_total(&wrong_currency, &eur),
        Err(crate::domain::model::RepoError::Money(
            bss_ledger_sdk::MoneyError::CurrencyMismatch
        ))
    ));
    let empty = super::settled_total(&[], &eur).unwrap();
    assert_eq!(empty.amount(), rust_decimal::Decimal::ZERO);
    assert_eq!(empty.currency(), &eur);
}

#[test]
fn settlement_total_narrows_only_after_all_cancellation() {
    let eur = bss_ledger_sdk::CurrencySpec::try_new("EUR".to_owned(), 2).unwrap();
    let max = "99999999999999999999999999.99";
    let lines = [
        money_line(max, "EUR", 2, "CR"),
        money_line(max, "EUR", 2, "CR"),
        money_line(max, "EUR", 2, "DR"),
    ];
    assert_eq!(
        super::settled_total(&lines, &eur)
            .unwrap()
            .amount()
            .to_string(),
        max
    );
    assert!(matches!(
        super::settled_total(&lines[..2], &eur),
        Err(crate::domain::model::RepoError::Money(
            bss_ledger_sdk::MoneyError::AmountOutOfRange
        ))
    ));
}

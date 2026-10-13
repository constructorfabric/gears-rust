//! Pure-fn unit tests for the audit-pack CSV encoder (DE1101: tests live in the
//! sibling `_tests.rs` hooked via `#[path]`). The scoped DB reads are covered by
//! the Postgres integration test `tests/postgres_inquiry.rs`.

use super::{CSV_HEADER, EntryRow, LineRow, csv_escape, push_row};

/// `csv_escape` returns a `Cow`; compare its `&str` view for clarity.
fn esc(field: &str) -> String {
    csv_escape(field).into_owned()
}

#[test]
fn plain_field_is_returned_verbatim() {
    assert_eq!(esc("REVENUE"), "REVENUE");
    assert_eq!(esc(""), "");
    assert_eq!(esc("a-b_c.123"), "a-b_c.123");
}

#[test]
fn field_with_comma_is_quoted() {
    assert_eq!(esc("Acme, Inc."), "\"Acme, Inc.\"");
}

#[test]
fn field_with_quote_doubles_the_quote_and_wraps() {
    // `say "hi"` → `"say ""hi"""`
    assert_eq!(esc("say \"hi\""), "\"say \"\"hi\"\"\"");
}

#[test]
fn field_with_newline_is_quoted() {
    assert_eq!(esc("line1\nline2"), "\"line1\nline2\"");
    assert_eq!(esc("line1\r\nline2"), "\"line1\r\nline2\"");
}

#[test]
fn formula_lead_in_is_neutralized_with_a_single_quote() {
    // A field starting with a spreadsheet formula trigger is prefixed with `'`
    // so Excel / Sheets render it as literal text, not a formula.
    assert_eq!(esc("=cmd|'/c calc'!A1"), "'=cmd|'/c calc'!A1");
    assert_eq!(esc("+1+1"), "'+1+1");
    assert_eq!(esc("-2+3"), "'-2+3");
    assert_eq!(esc("@SUM(A1)"), "'@SUM(A1)");
    assert_eq!(esc("\tlead-tab"), "'\tlead-tab");
}

#[test]
fn formula_lead_in_that_also_needs_quoting_is_prefixed_inside_the_quotes() {
    // `=A1,B1` triggers BOTH the formula guard and comma-quoting → `"'=A1,B1"`.
    assert_eq!(esc("=A1,B1"), "\"'=A1,B1\"");
}

#[test]
fn formula_trigger_only_applies_to_the_first_character() {
    // A `-`/`@`/`+` that is not the first character is harmless and untouched.
    assert_eq!(esc("a-b_c.123"), "a-b_c.123");
    assert_eq!(esc("user@host"), "user@host");
}

#[test]
fn header_column_count_matches_row_field_count() {
    // 23 columns: 12 entry fields + 11 line fields (see `push_row`): the line's
    // money is `amount` (decimal text) + `currency` + `currency_scale`.
    assert_eq!(CSV_HEADER.split(',').count(), 23);
}

fn entry_row() -> EntryRow {
    EntryRow {
        entry_id: uuid::Uuid::from_u128(1),
        tenant_id: uuid::Uuid::from_u128(2),
        legal_entity_id: uuid::Uuid::from_u128(3),
        period_id: "202606".to_owned(),
        entry_currency: "USD".to_owned(),
        source_doc_type: "INVOICE_POST".to_owned(),
        source_business_id: "inv-1".to_owned(),
        reverses_entry_id: None,
        posted_at_utc: time::OffsetDateTime::UNIX_EPOCH,
        posted_by_actor_id: uuid::Uuid::from_u128(4),
        origin: "SYSTEM".to_owned(),
        correlation_id: uuid::Uuid::from_u128(5),
        created_seq: 7,
    }
}

fn line_row(amount: &str, currency: &str, scale: u8) -> LineRow {
    LineRow {
        line_id: uuid::Uuid::from_u128(10),
        entry_id: uuid::Uuid::from_u128(1),
        payer_tenant_id: uuid::Uuid::from_u128(2),
        account_id: uuid::Uuid::from_u128(11),
        account_class: "AR".to_owned(),
        gl_code: None,
        side: "DR".to_owned(),
        money: bss_ledger_sdk::PostedMoney::try_new(
            bss_ledger_sdk::parse_decimal(amount).unwrap(),
            bss_ledger_sdk::CurrencySpec::try_new(currency.to_owned(), scale).unwrap(),
        )
        .unwrap(),
        invoice_id: Some("inv-1".to_owned()),
        revenue_stream: None,
        legal_entity_id: None,
    }
}

/// The comma-split fields of one emitted row (the fixtures need no quoting).
fn fields(row: &str) -> Vec<String> {
    row.strip_suffix('\n')
        .expect("a row ends with a newline")
        .split(',')
        .map(ToOwned::to_owned)
        .collect()
}

#[test]
fn a_line_row_writes_canonical_amount_currency_and_scale() {
    let header: Vec<&str> = CSV_HEADER.split(',').collect();
    let column = |name: &str| header.iter().position(|h| *h == name).unwrap();
    for (amount, currency, scale, text) in [
        ("12.340", "USD", 2, "12.34"),
        ("1200", "JPY", 0, "1200"),
        ("0.00000001", "BTC", 8, "0.00000001"),
    ] {
        let mut row = String::new();
        push_row(
            &mut row,
            &entry_row(),
            Some(&line_row(amount, currency, scale)),
        );
        let cells = fields(&row);
        assert_eq!(cells.len(), header.len(), "{row}");
        assert_eq!(cells[column("amount")], text);
        assert_eq!(cells[column("currency")], currency);
        assert_eq!(cells[column("currency_scale")], scale.to_string());
        assert_eq!(cells[column("invoice_id")], "inv-1");
        assert_eq!(cells[column("created_seq")], "7");
    }
}

#[test]
fn an_entry_without_lines_writes_blank_line_columns_at_full_width() {
    let mut row = String::new();
    push_row(&mut row, &entry_row(), None);
    let cells = fields(&row);
    assert_eq!(cells.len(), CSV_HEADER.split(',').count(), "{row}");
    assert_eq!(cells[0], uuid::Uuid::from_u128(1).to_string());
    assert!(cells[12..].iter().all(String::is_empty), "{row}");
}

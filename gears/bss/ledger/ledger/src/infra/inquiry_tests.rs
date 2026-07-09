//! Pure-fn unit tests for the audit-pack CSV encoder (DE1101: tests live in the
//! sibling `_tests.rs` hooked via `#[path]`). The scoped DB reads are covered by
//! the Postgres integration test `tests/postgres_inquiry.rs`.

use super::{CSV_HEADER, csv_escape};

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
    // 22 columns: 12 entry fields + 10 line fields (see `push_row`).
    assert_eq!(CSV_HEADER.split(',').count(), 22);
}

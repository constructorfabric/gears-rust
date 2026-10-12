#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;

#[test]
fn check_bodies_finds_every_check_and_respects_quotes() {
    let ddl = "CREATE TABLE t (\n  kind text CHECK (kind IN ('a)','b')), n integer NOT NULL,\n  \
               ref_kind text NOT NULL check(ref_kind IN ('price','plan_item')), recheck text)";

    assert_eq!(
        check_bodies(ddl),
        vec![
            "(kind IN ('a)','b'))",
            "(ref_kind IN ('price','plan_item'))"
        ]
    );
}

#[test]
fn only_a_ref_kind_check_naming_price_book_entry_admits_it() {
    let today = "(ref_kind IN ('price_book_entry','plan_item','sold_as'))".to_owned();
    let before = "(ref_kind IN ('price','plan_item','sold_as'))".to_owned();
    let postgres =
        "CHECK ((ref_kind = ANY (ARRAY['price_book_entry'::text, 'plan_item'::text])))".to_owned();

    assert!(admits_price_book_entry(std::slice::from_ref(&today)));
    assert!(admits_price_book_entry(std::slice::from_ref(&postgres)));
    assert!(!admits_price_book_entry(std::slice::from_ref(&before)));
    assert!(!admits_price_book_entry(&[today, before]));
    assert!(admits_price_book_entry(&[]), "no CHECK refuses no kind");
}

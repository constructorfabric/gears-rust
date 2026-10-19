//! Golden JSON fixtures of the V1 SDK DTOs (DESIGN §3.3, T-D-71). A non-additive change to any of
//! them fails here; record a new DTO's fixture with `UPDATE_RATING_GOLDEN=1`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use bss_rating_sdk::golden::{assert_additive, assert_census};
use bss_rating_sdk::{ExactMoney, ExactQuantity, Page, PageCursor, PageInfo, PageLimit};

// @cpt-begin:cpt-cf-bss-rating-algo-foundation-contract-gates:p2:inst-cg-golden
const FIXTURES: &[&str] = &["exact_money", "exact_quantity", "page"];

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/v1")
}

#[test]
fn every_fixture_is_checked() {
    assert_census(&dir(), FIXTURES);
}

#[test]
fn exact_money_v1() {
    // 0.1175 EUR as the reduced fraction 47/400, posting scale 2.
    let amount = ExactMoney::new("47", "400", "EUR", 2).unwrap();
    assert_additive(&dir(), "exact_money", &amount);
}

#[test]
fn exact_quantity_v1() {
    let quantity = ExactQuantity::parse("744.500").unwrap();
    assert_additive(&dir(), "exact_quantity", &quantity);
}

#[test]
fn page_v1() {
    let page = Page::new(
        vec![
            ExactQuantity::parse("1").unwrap(),
            ExactQuantity::parse("0.25").unwrap(),
        ],
        PageInfo::new(
            Some(PageCursor::new("eyJrIjoiMDAwMiJ9").unwrap()),
            PageLimit::new(Some(2)).unwrap(),
        ),
    );
    assert_additive(&dir(), "page", &page);
}
// @cpt-end:cpt-cf-bss-rating-algo-foundation-contract-gates:p2:inst-cg-golden

//! The golden consumer contracts on `SQLite` (run 4.4): the one body is `contract_support`, the
//! same one `postgres_contract.rs` runs. This tier alone re-records: `UPDATE_CONTRACT_GOLDEN=1`
//! rewrites `tests/contract/<golden>.json` from the doors' answers — a claim that the contract
//! was MEANT to change, which the diff has to justify.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#[macro_use]
mod contract_support;
mod plan_support;

async fn check(golden: &str) {
    let (f, catalog) = plan_support::setup().await;
    let world = contract_support::world(f, &catalog).await;
    let record = std::env::var("UPDATE_CONTRACT_GOLDEN").is_ok();
    contract_support::verify(&world, golden, record).await;
}

with_goldens!(contract_tests![]);

/// A golden no test reads is a contract nobody checks: `tests/contract/` holds exactly the
/// goldens of `contract_support`'s one list, which both tiers expand into their tests.
#[test]
fn every_golden_file_is_a_checked_contract() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/contract");
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    let mut checked: Vec<String> = contract_support::GOLDENS
        .iter()
        .map(|g| format!("{g}.json"))
        .collect();
    checked.sort();
    assert_eq!(
        files, checked,
        "every file in tests/contract/ is a checked golden"
    );
}

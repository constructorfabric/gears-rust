// Not under coverage: the nightly compiler of the coverage job adds diagnostics the pinned stable
// `.stderr` snapshots don't have (a `yansi::paint::Paint` import hint); the stable test lanes
// still run it. Same gate as the trybuild suites in toolkit-db-macros and event-broker-sdk.
#[test]
#[cfg(not(coverage_nightly))]
fn discovery_authority_cannot_be_constructed_or_used_as_write_authority() {
    let cases = trybuild::TestCases::new();
    // D-184 blocking gate on the production runtime capability types.
    cases.compile_fail("tests/ui/runtime_capability_escape.rs");
    cases.compile_fail("tests/ui/runtime_capability_private.rs");
}

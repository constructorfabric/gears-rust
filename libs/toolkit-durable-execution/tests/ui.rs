// Skipped under coverage, like every other trybuild suite in the workspace:
// the coverage lane runs a nightly, whose diagnostics are not the stable ones
// the `.stderr` files record.
#[cfg(not(coverage_nightly))]
#[test]
fn typed_workflows_reject_incompatible_inputs() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/*.rs");
    tests.compile_fail("tests/ui/*.rs");
}

//! Compile-time contract of `#[client_manual]` clients, across a real crate
//! boundary: the cases link this crate as an SDK dependency.

// Skipped under coverage, like every other trybuild suite in the workspace:
// the coverage lane's nightly diagnostics differ from the `.stderr` files.
#[cfg(not(coverage_nightly))]
#[test]
fn manual_client_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass/*.rs");
    t.compile_fail("tests/ui/fail/*.rs");
}

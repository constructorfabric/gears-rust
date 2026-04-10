#![allow(clippy::unwrap_used, clippy::expect_used)]

#[test]
#[cfg(not(coverage_nightly))]
fn ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass/*.rs");
    t.compile_fail("tests/ui/fail/*.rs");
}

//! The ordinal type's negative contract: the operations it must not offer.
//!
//! Asserted by compilation rather than at runtime, because the absence of an
//! operator is not observable from a running test.

// Skipped under coverage, like every other trybuild suite in the workspace:
// the coverage lane runs a nightly, whose diagnostics are not the stable ones
// the `.stderr` files record.
#[cfg(not(coverage_nightly))]
#[test]
fn sequence_arithmetic_does_not_compile() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/trybuild/sequence/no_arithmetic.rs");
}

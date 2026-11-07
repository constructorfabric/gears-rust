// Skipped under coverage, like every other trybuild suite in the workspace:
// the coverage lane runs a nightly, whose diagnostics are not the stable ones
// the `.stderr` files record, and trybuild on that nightly no longer finds the
// crate's enabled features, so an `outbox` case builds without `outbox`.
#[cfg(not(coverage_nightly))]
#[test]
fn typestate_builder_compile_failures_are_checked() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/trybuild/producer/missing_direct_broker.rs");

    #[cfg(feature = "outbox")]
    {
        tests.compile_fail("tests/trybuild/producer/direct_rejects_outbox_registration.rs");
        tests.compile_fail("tests/trybuild/producer/outbox_register_consumes_the_setup.rs");
        tests.compile_fail("tests/trybuild/producer/outbox_setup_has_no_enqueue.rs");
        tests.compile_fail("tests/trybuild/producer/missing_outbox_queue.rs");
        tests.compile_fail("tests/trybuild/producer/outbox_envelope_is_private.rs");
    }
}

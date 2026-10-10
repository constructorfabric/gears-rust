#[test]
fn discovery_authority_cannot_be_constructed_or_used_as_write_authority() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/discovery_escape.rs");
    // D-184 blocking gate on the production runtime capability types.
    cases.compile_fail("tests/ui/runtime_capability_escape.rs");
    cases.compile_fail("tests/ui/runtime_capability_private.rs");
}

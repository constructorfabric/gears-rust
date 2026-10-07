#[test]
fn typed_workflows_reject_incompatible_inputs() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/*.rs");
    tests.compile_fail("tests/ui/*.rs");
}

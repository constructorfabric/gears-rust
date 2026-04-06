use super::*;

#[test]
fn test_provider_trait_compiles() {
    let scope = AccessScope::default();
    assert!(scope.is_deny_all());
}

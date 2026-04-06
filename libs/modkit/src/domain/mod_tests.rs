use super::*;

// DomainModel is a simple marker trait - no trait bounds to test
#[allow(dead_code)]
fn assert_domain_model<T: DomainModel>() {}

#[test]
fn test_domain_model_trait_exists() {
    // Test that the trait is defined and can be used as a bound
    // Actual validation is done by the #[domain_model] macro
    fn _accepts_domain_model<T: DomainModel>(_: T) {}
}

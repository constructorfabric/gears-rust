use super::ConstructClientV1;

/// `ClientHub` stores the client as `Arc<dyn ConstructClientV1>`, so the trait
/// must stay dyn-compatible. This fails to compile if it does not.
#[test]
fn the_client_trait_is_dyn_compatible() {
    fn takes_a_trait_object(_: Option<&dyn ConstructClientV1>) {}
    takes_a_trait_object(None);
}

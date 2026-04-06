use super::*;
use uuid::{Uuid, uuid};

/// Test tenant ID for unit tests.
const TEST_TENANT_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000001");
/// Test subject ID for unit tests.
const TEST_SUBJECT_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000002");

fn test_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(TEST_SUBJECT_ID)
        .subject_tenant_id(TEST_TENANT_ID)
        .build()
        .unwrap()
}

struct MockClient {
    name: String,
}

impl MockClient {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
        }
    }

    fn get_name(&self) -> &str {
        &self.name
    }
}

#[test]
fn test_secured_new() {
    let client = MockClient::new("test-client");
    let ctx = test_ctx();

    let secured = Secured::new(&client, &ctx);

    assert_eq!(secured.client().get_name(), "test-client");
    assert_eq!(secured.ctx().subject_tenant_id(), ctx.subject_tenant_id());
}

#[test]
fn test_secured_getters() {
    let client = MockClient::new("test-client");
    let ctx = test_ctx();

    let secured = Secured::new(&client, &ctx);

    let client_ref = secured.client();
    assert_eq!(client_ref.get_name(), "test-client");

    let ctx_ref = secured.ctx();
    assert_eq!(ctx_ref.subject_tenant_id(), TEST_TENANT_ID);
}

#[test]
fn test_with_security_context_trait() {
    let client = MockClient::new("test-client");
    let ctx = test_ctx();

    let secured = client.security_ctx(&ctx);

    assert_eq!(secured.client().get_name(), "test-client");
    assert_eq!(secured.ctx().subject_tenant_id(), ctx.subject_tenant_id());
}

#[test]
fn test_secured_clone() {
    let client = MockClient::new("test-client");
    let ctx = test_ctx();

    let secured1 = client.security_ctx(&ctx);
    let secured2 = secured1;

    assert_eq!(secured1.client().get_name(), secured2.client().get_name());
    assert_eq!(
        secured1.ctx().subject_tenant_id(),
        secured2.ctx().subject_tenant_id()
    );
}

#[test]
fn test_secured_copy() {
    let client = MockClient::new("test-client");
    let ctx = test_ctx();

    let secured1 = client.security_ctx(&ctx);
    let secured2 = secured1;

    assert_eq!(secured1.client().get_name(), secured2.client().get_name());
    assert_eq!(
        secured1.ctx().subject_tenant_id(),
        secured2.ctx().subject_tenant_id()
    );
}

#[test]
fn test_secured_with_anonymous_context() {
    let client = MockClient::new("test-client");
    let ctx = SecurityContext::anonymous();

    let secured = client.security_ctx(&ctx);

    assert_eq!(secured.client().get_name(), "test-client");
    assert_eq!(secured.ctx().subject_tenant_id(), Uuid::default());
    assert_eq!(secured.ctx().subject_id(), Uuid::default());
}

#[test]
fn test_secured_with_custom_context() {
    let client = MockClient::new("test-client");
    let tenant_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let subject_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();

    let ctx = SecurityContext::builder()
        .subject_tenant_id(tenant_id)
        .subject_id(subject_id)
        .subject_type("user")
        .build()
        .unwrap();

    let secured = client.security_ctx(&ctx);

    assert_eq!(secured.ctx().subject_tenant_id(), tenant_id);
    assert_eq!(secured.ctx().subject_id(), subject_id);
}

#[test]
fn test_secured_zero_allocation() {
    let client = MockClient::new("test-client");
    let ctx = test_ctx();

    let secured = client.security_ctx(&ctx);

    assert_eq!(
        std::mem::size_of_val(&secured),
        std::mem::size_of::<&MockClient>() + std::mem::size_of::<&SecurityContext>()
    );
}

#[test]
fn test_multiple_clients_with_same_context() {
    let client1 = MockClient::new("client-1");
    let client2 = MockClient::new("client-2");
    let ctx = test_ctx();

    let secured1 = client1.security_ctx(&ctx);
    let secured2 = client2.security_ctx(&ctx);

    assert_eq!(secured1.client().get_name(), "client-1");
    assert_eq!(secured2.client().get_name(), "client-2");
    assert_eq!(
        secured1.ctx().subject_tenant_id(),
        secured2.ctx().subject_tenant_id()
    );
}

#[test]
fn test_secured_preserves_lifetimes() {
    let client = MockClient::new("test-client");
    let ctx = test_ctx();

    let secured = client.security_ctx(&ctx);

    assert_eq!(secured.client().get_name(), "test-client");
    assert_eq!(secured.ctx().subject_tenant_id(), ctx.subject_tenant_id());
}

#[test]
fn test_secured_query_builder() {
    use crate::odata::Schema;

    #[derive(Copy, Clone, Eq, PartialEq, Debug)]
    enum TestField {
        #[allow(dead_code)]
        Name,
    }

    struct TestSchema;

    impl Schema for TestSchema {
        type Field = TestField;

        fn field_name(field: Self::Field) -> &'static str {
            match field {
                TestField::Name => "name",
            }
        }
    }

    let client = MockClient::new("test-client");
    let ctx = test_ctx();

    let secured = client.security_ctx(&ctx);
    let query_builder = secured.query::<TestSchema>();

    let query = query_builder.page_size(50).build();
    assert_eq!(query.limit, Some(50));
}

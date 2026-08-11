use super::*;

fn test_ctx(scopes: &[&str]) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .subject_type("user")
        .token_scopes(scopes.iter().map(|s| (*s).to_owned()).collect())
        .build()
        .expect("test ctx")
}

#[test]
fn canonicalizes_scopes_order_dedup_join() {
    assert_eq!(canonical_scopes("b a  a c"), "a b c");
    assert_eq!(canonical_scopes("   "), "");
    assert_eq!(canonical_scopes("x"), "x");
}

#[test]
fn scopes_hash_is_stable() {
    assert_eq!(scopes_hash("a b"), scopes_hash("a b"));
    assert_ne!(scopes_hash("a b"), scopes_hash("a c"));
}

#[test]
fn cache_key_hashes_canonical_scopes() {
    let req = MintCapabilityRequest {
        context_tenant: Uuid::from_u128(9),
        context_project_id: None,
        audience: "aud".to_owned(),
        operation: None,
        resource_type: None,
    };
    let c = build_cap_claims(&test_ctx(&["b", "a"]), &req, "iss", 300, 1000);
    let key = cache_key_for(&c);
    assert_eq!(key.scopes_hash, scopes_hash("a b"));
    assert_eq!(key.aud, "aud");
}

#[test]
fn cache_key_differs_on_operation_and_resource_type() {
    let key = |op: Option<&str>, rt: Option<&str>| {
        let req = MintCapabilityRequest {
            context_tenant: Uuid::from_u128(9),
            context_project_id: None,
            audience: "aud".to_owned(),
            operation: op.map(str::to_owned),
            resource_type: rt.map(str::to_owned),
        };
        cache_key_for(&build_cap_claims(&test_ctx(&["a"]), &req, "iss", 300, 1000))
    };
    // operation/resource_type are baked into the signed claims, so requests that
    // differ in either must not collapse onto the same cached token.
    assert_ne!(key(Some("read"), None), key(Some("write"), None));
    assert_ne!(key(None, Some("bucket")), key(None, Some("object")));
    assert_ne!(key(None, None), key(Some("read"), None));
    assert_eq!(
        key(Some("read"), Some("bucket")),
        key(Some("read"), Some("bucket"))
    );
}

#[test]
fn builds_cap_claims_from_context() {
    let req = MintCapabilityRequest {
        context_tenant: Uuid::from_u128(42),
        context_project_id: None,
        audience: "gts.cf.rms._.adapter.v1~acme.rms._.s3.v1".to_owned(),
        operation: None,
        resource_type: None,
    };
    let c = build_cap_claims(
        &test_ctx(&["b", "a"]),
        &req,
        "https://core.example.com/issuers/cap",
        300,
        1_000,
    );
    assert_eq!(c.aud, req.audience);
    assert_eq!(c.context_tenant, Uuid::from_u128(42));
    assert_eq!(c.scopes, "a b"); // canonicalized
    assert_eq!(c.exp - c.iat, 300);
    assert_eq!(c.iss, "https://core.example.com/issuers/cap");
}

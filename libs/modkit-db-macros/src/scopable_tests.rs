use super::*;

#[test]
fn test_snake_to_upper_camel() {
    assert_eq!(snake_to_upper_camel("tenant_id"), "TenantId");
    assert_eq!(snake_to_upper_camel("id"), "Id");
    assert_eq!(snake_to_upper_camel("owner_user_id"), "OwnerUserId");
    assert_eq!(snake_to_upper_camel("custom_col"), "CustomCol");
}

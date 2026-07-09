use super::*;
use crate::domain::message::{Message, MessageRole};
use crate::domain::service::variant_service::VariantEntry;
use chat_engine_sdk::models::VariantInfo;
use time::OffsetDateTime;

#[test]
fn list_variants_response_converts_from_listing() {
    let m1 = Message {
        message_id: Uuid::nil(),
        session_id: Uuid::nil(),
        tenant_id: None,
        user_id: None,
        parent_message_id: None,
        variant_index: 0,
        is_active: true,
        role: MessageRole::Assistant,
        parts: vec![MessagePart::text(Uuid::nil(), Uuid::nil(), 0, "hi")],
        file_ids: vec![],
        metadata: None,
        is_complete: true,
        is_hidden_from_user: false,
        is_hidden_from_backend: false,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };
    let info = VariantInfo {
        message_id: Uuid::nil(),
        variant_index: 0,
        total_variants: 1,
        is_active: true,
    };
    let listing = VariantListing {
        variants: vec![VariantEntry { message: m1, info }],
        current_index: Some(0),
    };
    let resp: ListVariantsResponse = listing.into();
    assert_eq!(resp.current_index, Some(0));
    assert_eq!(resp.variants.len(), 1);
    assert!(resp.variants[0].is_active);
    assert_eq!(resp.variants[0].variant_index, 0);
}

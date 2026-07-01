use super::*;

/// Object-safety smoke test: confirms `Arc<dyn ReactionRepo>` compiles
/// with the methods declared on the trait, mirroring the pattern used
/// in other repository modules.
#[test]
fn trait_is_object_safe() {
    struct Stub;

    #[async_trait]
    impl ReactionRepo for Stub {
        async fn get_by_pk(
            &self,
            _message_id: Uuid,
            _user_id: &str,
        ) -> Result<Option<MessageReaction>, ChatEngineError> {
            Ok(None)
        }

        async fn upsert(
            &self,
            _message_id: Uuid,
            _user_id: &str,
            _reaction_type: ReactionType,
        ) -> Result<ReactionUpsertOutcome, ChatEngineError> {
            unreachable!()
        }

        async fn delete(
            &self,
            _message_id: Uuid,
            _user_id: &str,
        ) -> Result<ReactionDeleteOutcome, ChatEngineError> {
            Ok(ReactionDeleteOutcome {
                applied: false,
                previous_reaction_type: None,
            })
        }

        async fn list_by_message(
            &self,
            _message_id: Uuid,
        ) -> Result<Vec<MessageReaction>, ChatEngineError> {
            Ok(Vec::new())
        }
    }

    let _: std::sync::Arc<dyn ReactionRepo> = std::sync::Arc::new(Stub);
}

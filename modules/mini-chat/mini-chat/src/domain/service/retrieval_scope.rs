use std::collections::HashMap;

use uuid::Uuid;

/// Determines which documents are searched via `file_search` for a given turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalScope {
    /// Search only specific document attachment IDs.
    Scoped(Vec<Uuid>),
    /// Search all ready documents in the chat's vector store.
    AllChat,
    /// No documents available — `file_search` should not be included.
    None,
}

/// Resolves retrieval scope for a turn following RAG.md priority rules:
///
/// 1. If `rag_attachment_ids` is non-empty → search only those documents
/// 2. Else if `attachment_ids` contains documents → search only those
/// 3. Else if the chat has ready documents → search all (`AllChat`)
/// 4. Else → None (no `file_search`)
pub fn resolve_retrieval_scope(
    rag_attachment_ids: &[Uuid],
    attachment_ids: &[Uuid],
    attachment_kinds: &HashMap<Uuid, AttachmentKind>,
    chat_has_ready_documents: bool,
) -> RetrievalScope {
    // Priority 1: explicit rag_attachment_ids
    if !rag_attachment_ids.is_empty() {
        return RetrievalScope::Scoped(rag_attachment_ids.to_vec());
    }

    // Priority 2: document IDs from attachment_ids
    let doc_ids: Vec<Uuid> = attachment_ids
        .iter()
        .filter(|id| attachment_kinds.get(id) == Some(&AttachmentKind::Document))
        .copied()
        .collect();

    if !doc_ids.is_empty() {
        return RetrievalScope::Scoped(doc_ids);
    }

    // Priority 3: all ready documents in the chat
    if chat_has_ready_documents {
        return RetrievalScope::AllChat;
    }

    RetrievalScope::None
}

/// Minimal attachment kind enum for retrieval scope resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Document,
    Image,
}

impl AttachmentKind {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "document" => Some(Self::Document),
            "image" => Some(Self::Image),
            _ => Option::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn kinds(entries: &[(Uuid, AttachmentKind)]) -> HashMap<Uuid, AttachmentKind> {
        entries.iter().copied().collect()
    }

    // Scenario 1: document in attachment_ids, no rag_attachment_ids
    #[test]
    fn scenario_1_doc_in_attachment_ids() {
        let doc1 = uuid(1);
        let scope = resolve_retrieval_scope(
            &[],
            &[doc1],
            &kinds(&[(doc1, AttachmentKind::Document)]),
            true,
        );
        assert_eq!(scope, RetrievalScope::Scoped(vec![doc1]));
    }

    // Scenario 2: image in attachment_ids only — falls through to AllChat
    #[test]
    fn scenario_2_image_only() {
        let img1 = uuid(10);
        let scope =
            resolve_retrieval_scope(&[], &[img1], &kinds(&[(img1, AttachmentKind::Image)]), true);
        assert_eq!(scope, RetrievalScope::AllChat);
    }

    // Scenario 3: mixed image + document in attachment_ids
    #[test]
    fn scenario_3_mixed_image_and_doc() {
        let img1 = uuid(10);
        let doc1 = uuid(1);
        let scope = resolve_retrieval_scope(
            &[],
            &[img1, doc1],
            &kinds(&[
                (img1, AttachmentKind::Image),
                (doc1, AttachmentKind::Document),
            ]),
            true,
        );
        assert_eq!(scope, RetrievalScope::Scoped(vec![doc1]));
    }

    // Scenario 4: rag_attachment_ids provided
    #[test]
    fn scenario_4_rag_attachment_ids() {
        let doc1 = uuid(1);
        let doc2 = uuid(2);
        let scope = resolve_retrieval_scope(&[doc1, doc2], &[], &HashMap::new(), true);
        assert_eq!(scope, RetrievalScope::Scoped(vec![doc1, doc2]));
    }

    // Scenario 5: nothing specified, chat has documents → AllChat
    #[test]
    fn scenario_5_implicit_all_chat() {
        let scope = resolve_retrieval_scope(&[], &[], &HashMap::new(), true);
        assert_eq!(scope, RetrievalScope::AllChat);
    }

    // Scenario 6: rag_attachment_ids overrides attachment_ids
    #[test]
    fn scenario_6_rag_overrides_attachment() {
        let doc1 = uuid(1);
        let doc7 = uuid(7);
        let scope = resolve_retrieval_scope(
            &[doc7],
            &[doc1],
            &kinds(&[(doc1, AttachmentKind::Document)]),
            true,
        );
        assert_eq!(scope, RetrievalScope::Scoped(vec![doc7]));
    }

    // Scenario 7: no documents in chat → None
    #[test]
    fn scenario_7_no_documents() {
        let scope = resolve_retrieval_scope(&[], &[], &HashMap::new(), false);
        assert_eq!(scope, RetrievalScope::None);
    }

    // Image in attachment_ids with no documents in chat → None
    #[test]
    fn image_only_no_chat_docs() {
        let img1 = uuid(10);
        let scope = resolve_retrieval_scope(
            &[],
            &[img1],
            &kinds(&[(img1, AttachmentKind::Image)]),
            false,
        );
        assert_eq!(scope, RetrievalScope::None);
    }
}

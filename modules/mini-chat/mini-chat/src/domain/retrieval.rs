//! Retrieval mode decision engine.
//!
//! Pure function `determine_retrieval_mode()` maps inputs (kill switch, ready
//! doc count, message doc attachment IDs) to one of three modes. P1 ships
//! two-mode only (None | `UnrestrictedChatSearch`); `FilteredByAttachmentIds`
//! exists for P2 readiness.

use modkit_macros::domain_model;
use uuid::Uuid;

/// The retrieval strategy for a given turn.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalMode {
    /// No file search — kill switch active, no ready docs, or explicitly disabled.
    None,
    /// Search across all documents in the chat's vector store (no metadata filter).
    UnrestrictedChatSearch,
    /// Search restricted to specific attachment IDs via metadata filter.
    /// P2 only — not returned by P1 decision function.
    FilteredByAttachmentIds(Vec<Uuid>),
}

/// Determine the retrieval mode for a turn.
///
/// Pure function with no I/O. P1 ships two-mode: returns only `None` or
/// `UnrestrictedChatSearch`. The `message_doc_attachment_ids` are validated
/// and persisted in `message_attachments` but do not influence retrieval in P1.
///
/// # Arguments
/// * `file_search_disabled` — from CCM `kill_switches.disable_file_search`
/// * `ready_doc_count` — count of ready document attachments in the chat
/// * `_message_doc_attachment_ids` — doc attachment IDs in the message (P2 only)
#[must_use]
pub fn determine_retrieval_mode(
    file_search_disabled: bool,
    ready_doc_count: i64,
    _message_doc_attachment_ids: &[Uuid],
) -> RetrievalMode {
    if file_search_disabled {
        return RetrievalMode::None;
    }
    if ready_doc_count == 0 {
        return RetrievalMode::None;
    }
    // P1: always unrestricted. P2 will check _message_doc_attachment_ids
    // and return FilteredByAttachmentIds when non-empty.
    RetrievalMode::UnrestrictedChatSearch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_switch_disables_all() {
        assert_eq!(determine_retrieval_mode(true, 5, &[]), RetrievalMode::None,);
    }

    #[test]
    fn kill_switch_overrides_docs_and_ids() {
        let ids = vec![Uuid::nil()];
        assert_eq!(determine_retrieval_mode(true, 5, &ids), RetrievalMode::None,);
    }

    #[test]
    fn no_ready_docs_returns_none() {
        assert_eq!(determine_retrieval_mode(false, 0, &[]), RetrievalMode::None,);
    }

    #[test]
    fn zero_docs_with_ids_returns_none() {
        // ready_doc_count gate takes precedence
        let ids = vec![Uuid::nil()];
        assert_eq!(
            determine_retrieval_mode(false, 0, &ids),
            RetrievalMode::None,
        );
    }

    #[test]
    fn docs_exist_no_ids_returns_unrestricted() {
        assert_eq!(
            determine_retrieval_mode(false, 3, &[]),
            RetrievalMode::UnrestrictedChatSearch,
        );
    }

    #[test]
    fn docs_exist_with_ids_returns_unrestricted_in_p1() {
        // P1 two-mode: ignores message_doc_attachment_ids
        let ids = vec![Uuid::nil(), Uuid::nil()];
        assert_eq!(
            determine_retrieval_mode(false, 5, &ids),
            RetrievalMode::UnrestrictedChatSearch,
        );
    }

    #[test]
    fn single_doc_returns_unrestricted() {
        assert_eq!(
            determine_retrieval_mode(false, 1, &[]),
            RetrievalMode::UnrestrictedChatSearch,
        );
    }
}

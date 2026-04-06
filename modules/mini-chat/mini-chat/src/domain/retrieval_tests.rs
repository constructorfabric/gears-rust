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

use github_mirror::domain::scope::InlineCommentSnippets;

const HUNK: &str = "@@ -1,4 +1,4 @@\n a\n-b\n+c\n d";

fn snippets(before: i32, after: i32) -> InlineCommentSnippets {
    InlineCommentSnippets { before, after }
}

#[test]
fn a_count_keeps_that_many_lines_above_the_commented_line_without_diff_markers() {
    assert_eq!(
        snippets(2, 0).cut(Some(HUNK)),
        (Some("b\nc".to_owned()), None)
    );
    assert_eq!(
        snippets(9, 0).cut(Some(HUNK)),
        (Some("a\nb\nc".to_owned()), None)
    );
}

#[test]
fn minus_one_keeps_the_whole_side_and_zero_keeps_nothing() {
    assert_eq!(
        snippets(-1, -1).cut(Some(HUNK)),
        (Some("a\nb\nc".to_owned()), None)
    );
    assert_eq!(snippets(0, 0).cut(Some(HUNK)), (None, None));
}

#[test]
fn a_missing_or_header_only_hunk_yields_nothing() {
    assert_eq!(snippets(2, 2).cut(None), (None, None));
    assert_eq!(snippets(2, 2).cut(Some("@@ -1 +1 @@")), (None, None));
}

#[test]
fn the_after_side_stays_empty_because_a_hunk_ends_at_the_commented_line() {
    assert_eq!(snippets(0, -1).cut(Some(HUNK)), (None, None));
    assert_eq!(snippets(0, 3).cut(Some(HUNK)), (None, None));
}

#[test]
fn only_minus_one_or_a_line_count_is_accepted() {
    assert!(snippets(-1, 0).validate().is_ok());
    assert!(snippets(3, 4).validate().is_ok());
    assert!(snippets(-2, 0).validate().is_err());
    assert!(snippets(0, -3).validate().is_err());
}

#[test]
fn a_wider_request_is_not_covered_by_a_narrower_sync() {
    assert!(snippets(-1, -1).covers(snippets(2, 0)));
    assert!(snippets(3, 0).covers(snippets(2, 0)));
    assert!(!snippets(2, 0).covers(snippets(3, 0)));
    assert!(!snippets(5, 5).covers(snippets(-1, 0)));
}

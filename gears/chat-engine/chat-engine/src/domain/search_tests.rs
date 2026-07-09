use super::*;
use time::OffsetDateTime;

#[test]
fn search_query_effective_top_defaults_and_clamps() {
    let q = SearchQuery::default();
    assert_eq!(q.effective_top(), DEFAULT_PAGE_SIZE);

    let q = SearchQuery {
        top: Some(1000),
        ..Default::default()
    };
    assert_eq!(q.effective_top(), MAX_PAGE_SIZE);

    let q = SearchQuery {
        top: Some(0),
        ..Default::default()
    };
    assert_eq!(q.effective_top(), DEFAULT_PAGE_SIZE);

    let q = SearchQuery {
        top: Some(7),
        ..Default::default()
    };
    assert_eq!(q.effective_top(), 7);
}

#[test]
fn search_query_effective_skip_defaults_to_zero() {
    let q = SearchQuery::default();
    assert_eq!(q.effective_skip(), 0);
    let q = SearchQuery {
        skip: Some(42),
        ..Default::default()
    };
    assert_eq!(q.effective_skip(), 42);
}

#[test]
fn search_query_context_radius_clamped() {
    let q = SearchQuery {
        context_radius: Some(50),
        ..Default::default()
    };
    assert_eq!(q.effective_context_radius(), 5);
    let q = SearchQuery {
        context_radius: None,
        ..Default::default()
    };
    assert_eq!(q.effective_context_radius(), DEFAULT_CONTEXT_RADIUS);
}

#[test]
fn sanitize_strips_tsquery_operators() {
    let s = sanitize_for_tsquery("hello & world | foo!");
    // Operators are dropped and surrounding whitespace collapses to a
    // single space (consecutive whitespace runs are coalesced).
    assert_eq!(s, "hello world foo");
}

#[test]
fn sanitize_drops_parens_and_quotes() {
    let s = sanitize_for_tsquery("(quick \"brown\" fox)");
    assert_eq!(s, "quick brown fox");
}

#[test]
fn sanitize_collapses_whitespace() {
    let s = sanitize_for_tsquery("   foo \t\n bar   ");
    assert_eq!(s, "foo bar");
}

#[test]
fn escape_like_escapes_wildcards() {
    assert_eq!(escape_like_pattern("100%"), "100\\%");
    assert_eq!(escape_like_pattern("a_b"), "a\\_b");
    assert_eq!(escape_like_pattern("a\\b"), "a\\\\b");
    assert_eq!(escape_like_pattern("plain"), "plain");
}

#[test]
fn snippet_centers_on_match() {
    let content = "the quick brown fox jumps over the lazy dog and runs across the field";
    let s = make_snippet(content, "brown");
    assert!(s.contains("brown"));
}

#[test]
fn snippet_falls_back_on_no_match() {
    let content = "short body";
    let s = make_snippet(content, "missing");
    assert_eq!(s, "short body");
}

#[test]
fn extract_text_from_sdk_shape() {
    let v = serde_json::json!({ "text": "hello world" });
    assert_eq!(extract_searchable_text(&v), "hello world");
}

#[test]
fn extract_text_from_nested_parts() {
    let v = serde_json::json!([{ "text": "alpha" }, { "text": "beta" }]);
    assert_eq!(extract_searchable_text(&v), "alpha beta");
}

#[test]
fn extract_text_handles_unknown_shape() {
    let v = serde_json::json!({ "foo": 42 });
    assert_eq!(extract_searchable_text(&v), "");
}

#[test]
fn cursor_round_trip() {
    let ts = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(123);
    let c = Cursor::new(0.42, Uuid::nil(), ts);
    let encoded = c.encode();
    let decoded = Cursor::decode(&encoded).unwrap();
    assert_eq!(decoded.message_id, Uuid::nil());
    assert!((decoded.rank - 0.42).abs() < 1e-5);
    assert_eq!(
        decoded.created_at,
        Some(ts),
        "round-tripped cursor must preserve the created_at sort key",
    );
}

#[test]
fn cursor_decodes_legacy_format_without_created_at() {
    // Cursors minted before the `:t:<unix_ns>` tail was added MUST
    // still decode — clients in flight at the cutover hold them.
    let decoded = Cursor::decode("r:0.42:m:00000000-0000-0000-0000-000000000000").unwrap();
    assert_eq!(decoded.message_id, Uuid::nil());
    assert!(
        decoded.created_at.is_none(),
        "legacy cursor must surface as created_at=None so the backend falls \
         back to the position-based skip path",
    );
}

#[test]
fn cursor_rejects_malformed_input() {
    assert!(Cursor::decode("garbage").is_err());
    assert!(Cursor::decode("r:notafloat:m:nil").is_err());
    assert!(Cursor::decode("x:1.0:m:00000000-0000-0000-0000-000000000000").is_err());
    // Trailing junk past the optional `:t:<unix_ns>` tail.
    assert!(Cursor::decode("r:0:m:00000000-0000-0000-0000-000000000000:t:0:bogus").is_err());
    // `t` segment without a value.
    assert!(Cursor::decode("r:0:m:00000000-0000-0000-0000-000000000000:t:").is_err());
    // `t` segment with a non-numeric value.
    assert!(Cursor::decode("r:0:m:00000000-0000-0000-0000-000000000000:t:notanint").is_err());
}

#[test]
fn search_error_maps_to_chat_engine_error() {
    let err: ChatEngineError = SearchError::QueryRequired.into();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));

    let err: ChatEngineError = SearchError::QueryTooLong.into();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));

    let err: ChatEngineError = SearchError::SessionNotFound.into();
    assert!(matches!(err, ChatEngineError::NotFound { .. }));

    let err: ChatEngineError = SearchError::Forbidden.into();
    assert!(matches!(err, ChatEngineError::Forbidden { .. }));

    let err: ChatEngineError =
        SearchError::Backend(Box::new(ChatEngineError::internal("boom"))).into();
    assert!(matches!(err, ChatEngineError::Internal { .. }));
}

#[test]
fn message_ref_serializes_with_rfc3339() {
    let r = MessageRef {
        message_id: Uuid::nil(),
        role: MessageRole::User,
        parts: vec![MessagePart::text(Uuid::nil(), Uuid::nil(), 0, "x")],
        created_at: OffsetDateTime::UNIX_EPOCH,
    };
    let s = serde_json::to_string(&r).unwrap();
    assert!(s.contains("1970-01-01"));
}

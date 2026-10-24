#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;

#[test]
fn the_limit_defaults_to_one_hundred() {
    assert_eq!(PageLimit::new(None).unwrap().get(), 100);
    assert_eq!(PageLimit::default().get(), 100);
}

#[test]
fn the_limit_is_one_to_five_hundred() {
    assert_eq!(PageLimit::new(Some(1)).unwrap().get(), 1);
    assert_eq!(PageLimit::new(Some(500)).unwrap().get(), 500);
    assert_eq!(PageLimit::new(Some(0)), Err(PagingError::LimitOutOfRange));
    assert_eq!(PageLimit::new(Some(501)), Err(PagingError::LimitOutOfRange));
    assert_eq!(
        PageLimit::new(Some(u32::MAX)),
        Err(PagingError::LimitOutOfRange)
    );
    assert!(serde_json::from_str::<PageLimit>("501").is_err());
}

#[test]
fn a_cursor_is_url_safe_base64() {
    assert!(PageCursor::new("eyJrIjoxfQ").is_ok());
    assert!(PageCursor::new("a-b_c").is_ok());
    for bad in ["", "a=b", "a+b", "a/b", "a b"] {
        assert_eq!(
            PageCursor::new(bad),
            Err(PagingError::InvalidCursor),
            "{bad:?}"
        );
    }
    assert_eq!(
        PageCursor::new("a".repeat(MAX_CURSOR_LEN + 1)),
        Err(PagingError::InvalidCursor)
    );
}

#[test]
fn the_last_page_has_no_next_cursor() {
    let page = Page {
        items: vec![1, 2],
        page_info: PageInfo {
            next_cursor: None,
            limit: PageLimit::default(),
        },
    };
    let json = serde_json::to_value(&page).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"items": [1, 2], "page_info": {"limit": 100}})
    );
    let mapped = page.map_items(|n| n * 10);
    assert_eq!(mapped.items, vec![10, 20]);
}

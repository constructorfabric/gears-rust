#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
#[test]
fn categories_need_nonblank_code_and_name() {
    let mut new = NewCategory {
        code: " ".into(),
        name: String::new(),
        is_default: false,
        sort_order: 0,
    };
    assert_eq!(validate_new_category(&new).violations().len(), 2);
    new.code = "hosting".into();
    new.name = "Hosting".into();
    assert!(validate_new_category(&new).is_empty());
}

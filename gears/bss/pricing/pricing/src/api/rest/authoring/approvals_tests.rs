//! The book identity a served snapshot gains (D-516).
use super::*;
use serde_json::json;

/// D-516 (review RF-P item 7): the identity goes beside a `book_id` only where no `book` is
/// already there; a snapshot's own `book` is never replaced.
#[test]
fn a_book_already_beside_its_id_is_kept() {
    let id = Uuid::from_u128(0x516);
    let identity = json!({"id": id, "code": "eur", "name": "EUR book", "currency": "EUR"});
    let books = BTreeMap::from([(id, identity.clone())]);
    let own = json!({"id": id, "code": "own"});
    let mut snapshot = json!({
        "book_id": id,
        "prices": [{"book_id": id, "book": own}, {"book_id": "not an id"}],
    });
    attach_book_identity(&mut snapshot, &books);
    assert_eq!(snapshot["book"], identity, "{snapshot}");
    assert_eq!(snapshot["prices"][0]["book"], own, "{snapshot}");
    assert!(snapshot["prices"][1].get("book").is_none(), "{snapshot}");
}

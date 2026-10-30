//! The numbering of a value's text pieces: [`text_pieces`] and
//! [`text_piece_mut`] must name the same piece by the same index, for any JSON.

use proptest::prelude::*;
use serde_json::{Map, Value};

use crate::domain::plan::{PieceMut, text_chars_up_to, text_piece_mut, text_pieces};

/// Any JSON value: strings, numbers, booleans, nulls, nested arrays and
/// objects, with keys that need escaping in a JSON pointer.
fn any_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        "[a-z /~]{0,6}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::btree_map("[a-z/~]{0,4}", inner, 0..6)
                .prop_map(|fields| Value::Object(fields.into_iter().collect::<Map<_, _>>())),
        ]
    })
}

/// The text of a piece the mutable walk found.
fn text_of(piece: PieceMut<'_>, number: &str) -> String {
    match piece {
        PieceMut::Text(text) => text.clone(),
        PieceMut::Number => number.to_owned(),
    }
}

proptest! {
    #[test]
    fn both_walks_name_the_same_piece_by_the_same_index(value in any_value()) {
        let texts = text_pieces(&value);
        for (index, text) in texts.iter().enumerate() {
            let mut copy = value.clone();
            let piece = text_piece_mut(&mut copy, index);
            prop_assert!(piece.is_some(), "piece {index} of {value} not found");
            if let Some(piece) = piece {
                prop_assert_eq!(&text_of(piece, text), text);
            }
        }
        let mut past_the_end = value;
        prop_assert!(text_piece_mut(&mut past_the_end, texts.len()).is_none());
    }

    #[test]
    fn the_character_count_is_the_sum_of_the_pieces(value in any_value()) {
        let total: usize = text_pieces(&value).iter().map(|text| text.chars().count()).sum();
        prop_assert_eq!(text_chars_up_to(&value, usize::MAX), total);
    }
}

#[test]
fn a_string_piece_is_editable_and_a_number_piece_is_not() {
    let mut value = serde_json::json!({ "name": "Ada", "year": 1815 });

    match text_piece_mut(&mut value, 0) {
        Some(PieceMut::Text(text)) => *text = "A.".to_owned(),
        other => panic!("piece 0 is the name, got {other:?}"),
    }
    assert!(matches!(
        text_piece_mut(&mut value, 1),
        Some(PieceMut::Number)
    ));

    assert_eq!(value, serde_json::json!({ "name": "A.", "year": 1815 }));
}

#[test]
fn the_character_count_stops_once_it_passes_the_limit() {
    let value = serde_json::json!(["abc", "defg", "hijkl"]);

    // "abc" alone passes 2; "abc" and "defg" pass 5; nothing passes 100.
    assert_eq!(text_chars_up_to(&value, 2), 3);
    assert_eq!(text_chars_up_to(&value, 5), 7);
    assert_eq!(text_chars_up_to(&value, 100), 12);
}

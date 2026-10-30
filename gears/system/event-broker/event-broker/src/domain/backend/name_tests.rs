use super::{InvalidName, Name};

#[test]
fn a_name_follows_the_cluster_profile_name_rule() {
    for valid in ["main", "audit-2", "EU_west", &"a".repeat(255)] {
        assert_eq!(Name::parse(valid).map(String::from), Ok(valid.to_owned()));
    }
    for invalid in ["", "has space", "a/b", "dsn://x", &"a".repeat(256)] {
        assert_eq!(Name::parse(invalid), Err(InvalidName), "{invalid:?}");
    }
}

/// A persisted name is checked on the way back in, like one read from
/// configuration.
#[test]
fn a_name_round_trips_through_serde_and_refuses_an_invalid_one() {
    let name = Name::parse("audit").expect("a valid name");
    assert_eq!(
        serde_json::to_value(&name).expect("serializes"),
        serde_json::json!("audit")
    );
    assert_eq!(
        serde_json::from_value::<Name>(serde_json::json!("audit")).expect("deserializes"),
        name
    );
    assert_eq!(
        serde_json::from_value::<Name>(serde_json::json!("a b"))
            .expect_err("invalid")
            .to_string(),
        "a backend name is 1 to 255 characters of [a-zA-Z0-9_-]"
    );
}

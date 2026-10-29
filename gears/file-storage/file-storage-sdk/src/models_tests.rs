use super::*;

// The DB CHECK constraints rely on these exact spellings.
#[test]
fn owner_kind_parse_matches_db_spellings() {
    for (kind, s) in [(OwnerKind::User, "user"), (OwnerKind::App, "app")] {
        assert_eq!(kind.as_str(), s);
        assert_eq!(OwnerKind::parse(s), Some(kind));
    }
    assert_eq!(OwnerKind::parse("robot"), None);
}

#[test]
fn version_status_parse_matches_db_spellings() {
    for (status, s) in [
        (VersionStatus::Pending, "pending"),
        (VersionStatus::Available, "available"),
    ] {
        assert_eq!(status.as_str(), s);
        assert_eq!(VersionStatus::parse(s), Some(status));
    }
    assert_eq!(VersionStatus::parse("frozen"), None);
}

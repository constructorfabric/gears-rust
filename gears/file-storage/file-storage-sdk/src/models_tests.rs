use super::*;

#[test]
fn owner_kind_user_and_app_are_distinct() {
    assert_ne!(OwnerKind::User, OwnerKind::App);
}

#[test]
fn owner_kind_is_copy_and_eq() {
    let a = OwnerKind::User;
    let b = a; // Copy — `a` remains usable below
    assert_eq!(a, b);
    assert_eq!(OwnerKind::App, OwnerKind::App);
}

#[test]
fn owner_kind_debug_is_stable() {
    assert_eq!(format!("{:?}", OwnerKind::User), "User");
    assert_eq!(format!("{:?}", OwnerKind::App), "App");
}

#[test]
fn file_and_version_ids_are_distinct_uuid_aliases() {
    // FileId and VersionId are both Uuid aliases; the nil sentinel round-trips.
    let f: FileId = uuid::Uuid::nil();
    let v: VersionId = uuid::Uuid::nil();
    assert_eq!(f, v);
    assert!(f.is_nil());
}

#[test]
fn owner_kind_str_round_trips() {
    for kind in [OwnerKind::User, OwnerKind::App] {
        assert_eq!(OwnerKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(OwnerKind::parse("robot"), None);
}

#[test]
fn owner_kind_str_spellings_are_exact() {
    // DB CHECK constraint relies on these exact spellings.
    assert_eq!(OwnerKind::User.as_str(), "user");
    assert_eq!(OwnerKind::App.as_str(), "app");
}

#[test]
fn version_status_str_round_trips() {
    for s in [VersionStatus::Pending, VersionStatus::Available] {
        assert_eq!(VersionStatus::parse(s.as_str()), Some(s));
    }
    assert_eq!(VersionStatus::parse("frozen"), None);
}

#[test]
fn version_status_spellings_are_exact() {
    assert_eq!(VersionStatus::Pending.as_str(), "pending");
    assert_eq!(VersionStatus::Available.as_str(), "available");
}

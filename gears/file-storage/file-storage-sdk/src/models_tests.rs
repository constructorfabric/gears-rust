use super::*;

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

#[test]
fn bind_state_spellings_are_exact() {
    assert_eq!(BindState::Bound.as_str(), "bound");
    assert_eq!(BindState::Conflict.as_str(), "conflict");
    assert_eq!(BindState::Manual.as_str(), "manual");
}

#[test]
fn multipart_upload_state_str_round_trips() {
    for s in [
        MultipartUploadState::InProgress,
        MultipartUploadState::Completing,
        MultipartUploadState::Completed,
        MultipartUploadState::Aborted,
    ] {
        assert_eq!(MultipartUploadState::parse(s.as_str()), Some(s));
    }
    assert_eq!(MultipartUploadState::parse("unknown"), None);
}

#[test]
fn policy_scope_str_round_trips() {
    for s in [PolicyScope::Tenant, PolicyScope::User] {
        assert_eq!(PolicyScope::parse(s.as_str()), Some(s));
    }
    assert_eq!(PolicyScope::parse("bogus"), None);
}

#[test]
fn retention_scope_str_round_trips() {
    for s in [
        RetentionScope::Tenant,
        RetentionScope::User,
        RetentionScope::File,
    ] {
        assert_eq!(RetentionScope::parse(s.as_str()), Some(s));
    }
    assert_eq!(RetentionScope::parse("bogus"), None);
}

use std::sync::Arc;

use crate::hub;

#[test]
fn two_spellings_of_one_file_share_a_hub() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let plain = hub::of(&dir.path().join("events.db")).unwrap();
    let dotted = hub::of(&dir.path().join("sub/../events.db")).unwrap();
    assert!(Arc::ptr_eq(&plain, &dotted));
}

#[test]
fn different_files_have_different_hubs() {
    let dir = tempfile::tempdir().unwrap();
    let one = hub::of(&dir.path().join("one.db")).unwrap();
    let two = hub::of(&dir.path().join("two.db")).unwrap();
    assert!(!Arc::ptr_eq(&one, &two));
}

#[test]
fn a_hub_lives_only_while_an_instance_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.db");
    let first = Arc::downgrade(&hub::of(&path).unwrap());
    assert!(first.upgrade().is_none());
    let second = Arc::downgrade(&hub::of(&path).unwrap());
    assert!(second.upgrade().is_none());
}

#[test]
fn a_missing_directory_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(hub::of(&dir.path().join("missing/events.db")).is_err());
}

#[cfg(unix)]
#[test]
fn a_symlink_to_the_file_shares_its_hub() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("events.db");
    std::fs::write(&file, b"").unwrap();
    let link = dir.path().join("link.db");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    let direct = hub::of(&file).unwrap();
    let linked = hub::of(&link).unwrap();
    assert!(Arc::ptr_eq(&direct, &linked));
}

#[test]
fn a_file_created_after_its_hub_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("events.db");
    let before = hub::of(&file).unwrap();
    std::fs::write(&file, b"").unwrap();
    let after = hub::of(&file).unwrap();
    assert!(Arc::ptr_eq(&before, &after));
}

//! `engine_digest` covers the generation's frozen tree (rating ADR-0006, "Confirmation"): the
//! digest the crate is compiled with is the digest of its tree, and editing a vendored file, a
//! source file or a golden vector changes it.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;

#[path = "../build/tree_digest.rs"]
mod tree_digest;

use bss_rating_engine_gen_1::ENGINE_DIGEST;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A copy of the frozen tree in a fresh directory.
fn copy_tree() -> tempfile::TempDir {
    let copy = tempfile::tempdir().unwrap();
    for (relative, path) in tree_digest::frozen_files(root()).unwrap() {
        let target = copy.path().join(&relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(path, target).unwrap();
    }
    copy
}

fn append(path: &Path, text: &str) {
    let mut content = fs::read(path).unwrap();
    content.extend_from_slice(text.as_bytes());
    fs::write(path, content).unwrap();
}

#[test]
fn the_compiled_digest_is_the_digest_of_the_tree() {
    assert_eq!(tree_digest::tree_digest(root()).unwrap(), ENGINE_DIGEST);
    assert_eq!(ENGINE_DIGEST.len(), 64);
}

#[test]
fn editing_a_vendored_file_changes_the_digest() {
    let copy = copy_tree();
    assert_eq!(
        tree_digest::tree_digest(copy.path()).unwrap(),
        ENGINE_DIGEST
    );
    append(&copy.path().join("vendor/derived/src/derived.rs"), "\n");
    assert_ne!(
        tree_digest::tree_digest(copy.path()).unwrap(),
        ENGINE_DIGEST
    );
}

#[test]
fn sources_golden_vectors_and_the_lock_are_covered_and_tests_are_not() {
    for edited in [
        "src/pipeline.rs",
        "vendor/derived/src/derived.rs",
        "vendor/LOCK.toml",
        "golden/v38-flat-one-third.expected.json",
        "Cargo.toml",
    ] {
        let copy = copy_tree();
        append(&copy.path().join(edited), " ");
        assert_ne!(
            tree_digest::tree_digest(copy.path()).unwrap(),
            ENGINE_DIGEST,
            "{edited}"
        );
    }
    let copy = copy_tree();
    fs::create_dir_all(copy.path().join("tests")).unwrap();
    fs::write(copy.path().join("tests/extra.rs"), "// not semantics").unwrap();
    fs::write(copy.path().join("src/.DS_Store"), "finder").unwrap();
    assert_eq!(
        tree_digest::tree_digest(copy.path()).unwrap(),
        ENGINE_DIGEST
    );
}

#[test]
fn moving_a_file_changes_the_digest() {
    let copy = copy_tree();
    fs::rename(
        copy.path().join("golden/v38-flat-one-third.input.json"),
        copy.path()
            .join("golden/v38-flat-one-third-moved.input.json"),
    )
    .unwrap();
    assert_ne!(
        tree_digest::tree_digest(copy.path()).unwrap(),
        ENGINE_DIGEST
    );
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_tree_is_refused() {
    let copy = copy_tree();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), copy.path().join("src/linked.rs")).unwrap();
    let error = tree_digest::tree_digest(copy.path()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn a_missing_frozen_directory_or_root_file_is_refused() {
    for missing in ["golden", "vendor", "src", "build"] {
        let copy = copy_tree();
        fs::remove_dir_all(copy.path().join(missing)).unwrap();
        assert!(tree_digest::tree_digest(copy.path()).is_err(), "{missing}");
    }
    for missing in tree_digest::ROOT_FILES {
        let copy = copy_tree();
        fs::remove_file(copy.path().join(missing)).unwrap();
        assert!(tree_digest::tree_digest(copy.path()).is_err(), "{missing}");
    }
}

/// A file name that is not UTF-8 is refused, never skipped as if hidden. Linux only: APFS, the
/// macOS file system, cannot store such a name.
#[cfg(target_os = "linux")]
#[test]
fn a_file_name_that_is_not_utf8_is_refused() {
    use std::os::unix::ffi::OsStrExt;
    let copy = copy_tree();
    let name = std::ffi::OsStr::from_bytes(b"stray\xff.rs");
    fs::write(copy.path().join("src").join(name), "").unwrap();
    let error = tree_digest::tree_digest(copy.path()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

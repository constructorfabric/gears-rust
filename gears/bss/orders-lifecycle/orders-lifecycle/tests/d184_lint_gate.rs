//! D-184 blocking gate backstop: Clippy's `disallowed_methods` (crate-local clippy.toml) is the
//! primary check; this scan fails if the configuration drifts from the workspace file, the deny
//! attribute is removed, or `allow_all` appears outside the permitted discovery constructor.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Repository files must exist; fail loudly.
use std::path::{Path, PathBuf};

const NEEDLE: &str = concat!("allow", "_all(");
const PERMITTED: &[&str] = &[
    // The runtime read-only DiscoveryScope constructor.
    "orders-lifecycle/src/infra/maintenance/scope.rs",
    // The S1-03 capability prototype's identical constructor (test-only).
    "orders-lifecycle/tests/capability_support/capabilities.rs",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn crate_local_clippy_configs_carry_the_workspace_file_and_the_allow_all_ban() {
    let workspace = std::fs::read_to_string(root().join("../../../../clippy.toml")).unwrap();
    for krate in ["orders-lifecycle", "orders-lifecycle-sdk"] {
        let local =
            std::fs::read_to_string(root().join("..").join(krate).join("clippy.toml")).unwrap();
        for line in workspace
            .lines()
            .filter(|l| !l.trim().is_empty() && l.trim() != "]")
        {
            assert!(local.contains(line), "{krate}/clippy.toml drifted: {line}");
        }
        assert!(
            local.contains("toolkit_security::access_scope::AccessScope::allow_all"),
            "{krate}/clippy.toml lost the D-184 entry"
        );
        let lib =
            std::fs::read_to_string(root().join("..").join(krate).join("src/lib.rs")).unwrap();
        assert!(
            lib.contains("#![deny(clippy::disallowed_methods)]"),
            "{krate}"
        );
    }
}

#[test]
fn allow_all_appears_only_in_the_permitted_discovery_constructors() {
    let parent = root().join("..");
    let mut files = vec![];
    for krate in ["orders-lifecycle", "orders-lifecycle-sdk"] {
        for dir in ["src", "tests"] {
            let path = parent.join(krate).join(dir);
            if path.exists() {
                rust_files(&path, &mut files);
            }
        }
    }
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let hits = text.matches(NEEDLE).count();
        if hits == 0 {
            continue;
        }
        let relative = file
            .strip_prefix(&parent)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        assert!(
            PERMITTED.contains(&relative.as_str()),
            "AccessScope::{NEEDLE} outside the D-184 discovery constructor: {relative}"
        );
        assert_eq!(hits, 1, "{relative}");
        let before = &text[..text.find(NEEDLE).unwrap()];
        let attribute = before
            .rfind("#[allow(clippy::disallowed_methods)]")
            .unwrap();
        assert!(
            !before[attribute..].contains("\n}\n"),
            "{relative}: the allow must sit on the constructor itself"
        );
    }
}

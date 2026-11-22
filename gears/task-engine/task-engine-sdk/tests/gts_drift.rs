//! Drift check between `src/gts.rs` and the registered GTS entities in
//! `gears/task-engine/docs/schemas/` (`types/*.schema.json`,
//! `instances/*.json`).
//!
//! Every schema/instance file must have a constant, every constant must have a
//! file, and each file's own `$id` / `id` must match its file name. When this
//! fails, add or remove the constant (or the file) so both sides agree.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const GTS_PREFIX: &str = "gts.";
const GTS_URI_PREFIX: &str = "gts://";

fn docs_schemas() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/schemas")
}

/// GTS IDs of the files in one schema directory, by file name. Also checks
/// that each file declares the ID its name says (`id_field` is `$id` for type
/// schemas, `id` for instances).
fn ids_in(dir: &str, suffix: &str, id_field: &str) -> BTreeSet<String> {
    let path = docs_schemas().join(dir);
    let mut ids = BTreeSet::new();
    for entry in fs::read_dir(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())) {
        let file = entry.unwrap_or_else(|e| panic!("dir entry: {e}")).path();
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| panic!("non-UTF-8 file name: {}", file.display()));
        let Some(id) = name.strip_suffix(suffix) else {
            continue;
        };
        let json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {name}: {e}")),
        )
        .unwrap_or_else(|e| panic!("parse {name}: {e}"));
        let declared = json[id_field].as_str().unwrap_or_default();
        let declared = declared.strip_prefix(GTS_URI_PREFIX).unwrap_or(declared);
        assert_eq!(
            declared, id,
            "{dir}/{name}: `{id_field}` does not match the file name"
        );
        ids.insert(id.to_owned());
    }
    ids
}

/// Every `gts_id!("…")` literal in `src/gts.rs`, with the `gts.` prefix the
/// macro adds.
fn ids_in_gts_rs() -> BTreeSet<String> {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gts.rs"))
        .unwrap_or_else(|e| panic!("read src/gts.rs: {e}"));
    src.split("gts_id!(\"")
        .skip(1)
        .map(|rest| {
            let end = rest
                .find("\")")
                .unwrap_or_else(|| panic!("unterminated gts_id! literal in src/gts.rs"));
            format!("{GTS_PREFIX}{}", &rest[..end])
        })
        .collect()
}

#[test]
fn gts_constants_match_registered_schemas_and_instances() {
    let mut registered = ids_in("types", ".schema.json", "$id");
    registered.extend(ids_in("instances", ".json", "id"));
    let constants = ids_in_gts_rs();

    let missing: Vec<_> = registered.difference(&constants).collect();
    let extra: Vec<_> = constants.difference(&registered).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "src/gts.rs drifted from docs/schemas/\n  \
         registered but no constant: {missing:#?}\n  \
         constant but not registered: {extra:#?}"
    );
}

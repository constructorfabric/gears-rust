use super::*;
use std::fs;

/// Builds a throwaway corpus tree so the loader is tested independently of the
/// real corpus content, which later tasks add.
fn write_tree(root: &std::path::Path) {
    let fam = root.join("tier-boundary");
    fs::create_dir_all(&fam).unwrap();

    fs::write(
        fam.join("_family.toml"),
        r#"
family     = "tier-boundary"
role       = "publish"
gates      = ["graduated", "volume"]
provenance = ["AC#60"]
"#,
    )
    .unwrap();

    fs::write(
        fam.join("a-case.toml"),
        r#"
family     = "tier-boundary"
id         = "a-case"
kind       = "evaluation"
provenance = ["AC#60"]

[snapshot]
model_kind  = "graduated"
charge_kind = "usage"
currency    = "USD"

  [[snapshot.bands]]
  from_qty = 0
  to_qty   = "open"
  unit_amount_minor = 5

[[assert]]
given  = { q = 3 }
expect = { charge_minor = 15 }
"#,
    )
    .unwrap();
}

/// A corpus tree of this run's own, removed when the test that owns it ends.
///
/// The six trees were **fixed** names under `std::env::temp_dir()` until
/// 2026-08-20 (`bss-fixtures-loader-ok` and its siblings), and each test opened
/// by `remove_dir_all`-ing its own. Two concurrent runs of this crate therefore
/// deleted each other's tree mid-test -- a second worktree, a CI matrix leg, a
/// parallel session, all of which this repository has. Measured before the fix:
/// **21 of 48** runs across eight concurrent processes failed, on
/// `DirectoryNotEmpty` out of the clean-up and on `create_dir_all` inside
/// `write_tree`. A corpus-integrity suite that goes red for that reason has said
/// nothing about the corpus, which is the opposite of what it is for.
///
/// The pid separates processes and the counter separates trees within one, so
/// the directory cannot pre-exist and there is no opening clean-up to race.
struct TempTree(std::path::PathBuf);

impl TempTree {
    /// Build a fresh tree, named so no other process can be holding it.
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);

        let nth = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "bss-fixtures-loader-{label}-{}-{nth}",
            std::process::id()
        ));
        // Only reachable through pid reuse after a run that aborted without
        // unwinding, so no live process can be inside it and this cannot race
        // the way the fixed names did. `drop` rather than `let _ =` satisfies
        // clippy::let_underscore_must_use.
        drop(fs::remove_dir_all(&root));
        write_tree(&root);
        Self(root)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        // Best-effort, and deliberately so: this runs during the unwind of a
        // failing assertion too, and a panic here would abort the process and
        // bury the failure that caused it. `drop` rather than `let _ =`
        // satisfies clippy::let_underscore_must_use.
        drop(fs::remove_dir_all(&self.0));
    }
}

#[test]
fn loads_families_and_cases() {
    let tree = TempTree::new("ok");
    let tmp = tree.path();

    let corpus = Corpus::load(tmp).expect("corpus must load");

    assert_eq!(corpus.families.len(), 1);
    assert_eq!(
        corpus.families[0].gates,
        vec![ModelKind::Graduated, ModelKind::Volume]
    );
    assert_eq!(corpus.cases.len(), 1);
    assert_eq!(corpus.cases_for(Family::TierBoundary).count(), 1);
}

#[test]
fn a_parse_error_names_the_file() {
    let tree = TempTree::new("bad");
    let tmp = tree.path();
    fs::write(
        tmp.join("tier-boundary").join("broken.toml"),
        "family = \"nope\"",
    )
    .unwrap();

    let err = Corpus::load(tmp).expect_err("a broken case must fail the load");

    // The variant first: every `CorpusError`'s `#[error]` format embeds the path,
    // so the filename assertion below is satisfied by `Io`, `Misfiled`,
    // `UnknownFamilyDirectory` and `StrayRootFile` just as well as by the parse
    // failure this test is named for.
    assert!(
        matches!(err, CorpusError::Parse { .. }),
        "and it must be the parse failure rather than some other refusal: {err}"
    );
    assert!(
        err.to_string().contains("broken.toml"),
        "error must name the file, got: {err}"
    );
}

#[test]
fn a_case_filed_under_the_wrong_family_directory_is_rejected() {
    let tree = TempTree::new("misfiled");
    let tmp = tree.path();
    let misfiled = fs::read_to_string(tmp.join("tier-boundary").join("a-case.toml"))
        .unwrap()
        .replace(
            r#"family     = "tier-boundary""#,
            r#"family     = "package""#,
        );
    fs::write(tmp.join("tier-boundary").join("misfiled.toml"), misfiled).unwrap();

    let err = Corpus::load(tmp).expect_err("a misfiled case must fail the load");

    // The discriminator every guard shares is the path, so the filename alone
    // cannot tell this refusal from an incidental parse failure.
    assert!(
        matches!(err, CorpusError::Misfiled { .. }),
        "and it must be the misfiling rather than some other refusal: {err}"
    );
    assert!(
        err.to_string().contains("misfiled.toml"),
        "error must name the file, got: {err}"
    );
}

#[test]
fn the_real_corpus_loads() {
    Corpus::load(&Corpus::corpus_root()).expect("the committed corpus must load");
}

#[test]
fn a_case_file_dropped_at_the_corpus_root_is_refused_rather_than_skipped() {
    // The one open edge the loader had. The family walk takes directories only,
    // so a `.toml` at the root was skipped without comment: an authored case
    // that never runs, contributes no coverage, and lets the registry
    // regenerate cleanly as if it did not exist. Every other misfiling inside
    // the tree is already fail-closed, which is what made this one easy to miss.
    let tree = TempTree::new("stray-root");
    let tmp = tree.path();
    let stray = fs::read_to_string(tmp.join("tier-boundary").join("a-case.toml")).unwrap();
    fs::write(tmp.join("stray-at-root.toml"), stray).unwrap();

    let err = Corpus::load(tmp).expect_err("a case at the root must fail the load");

    assert!(
        matches!(err, CorpusError::StrayRootFile { .. }),
        "and it must be the root-file refusal rather than some incidental parse error: {err}"
    );
    assert!(
        err.to_string().contains("stray-at-root.toml"),
        "error must name the file, got: {err}"
    );
}

#[test]
fn the_generated_registry_at_the_root_is_the_one_permitted_file() {
    // The positive control the refusal above needs: `registry.toml` is the gate
    // file the generator writes beside the families, and refusing it would make
    // the committed corpus unloadable. `the_real_corpus_loads` covers that too,
    // but only incidentally -- this states it.
    let tree = TempTree::new("registry-at-root");
    let tmp = tree.path();
    fs::write(tmp.join("registry.toml"), "# generated\n").unwrap();

    Corpus::load(tmp).expect("registry.toml at the root must not be mistaken for a stray case");
}

#[test]
fn a_non_toml_file_at_the_root_is_not_refused() {
    // The refusal is scoped to `.toml`: a README, a `.gitignore` or an editor
    // swap file at the root is not an authored case and must not fail a load.
    let tree = TempTree::new("root-readme");
    let tmp = tree.path();
    fs::write(tmp.join("README.md"), "not a case\n").unwrap();

    Corpus::load(tmp).expect("a non-case file at the root must load cleanly");
}

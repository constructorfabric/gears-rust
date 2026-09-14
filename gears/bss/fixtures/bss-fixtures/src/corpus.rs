//! Loads the on-disk corpus.
//!
//! One directory per family; `_family.toml` declares what the family gates and
//! every other `.toml` is a case. A case whose `family` disagrees with its
//! directory is an error — the directory is the index, so the two must not
//! drift.
//!
//! The loader is **fail-closed at every edge**. A `.toml` other than the
//! generated `registry.toml` sitting at the corpus root is a
//! [`CorpusError::StrayRootFile`] rather than a file the walk quietly skips, and
//! one level down a family directory refuses both a nested directory
//! ([`CorpusError::NestedInFamily`]) and a non-`.toml` file
//! ([`CorpusError::StrayFamilyFile`]) for the same reason: each was a place an
//! authored case could exist, never run, and produce no error at all, with the
//! registry regenerating cleanly without it. Refusing is cheap and a
//! silently-dropped fixture is not — a corpus that reports coverage it does not
//! have is the failure this whole artifact exists to prevent.

use crate::model::{Case, CaseHeader, CaseKind, Family, ModelKind};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum CorpusError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("{path}: declares family {declared:?} but sits in the {directory:?} directory")]
    Misfiled {
        path: String,
        declared: Family,
        directory: Family,
    },
    #[error("{path}: directory name is not a known family")]
    UnknownFamilyDirectory { path: String },
    #[error(
        "{path}: a case file at the corpus root belongs in a family directory; \
         only registry.toml lives here"
    )]
    StrayRootFile { path: String },
    #[error(
        "{path}: a family directory holds case files and nothing else; a nested \
         directory is never walked, so a case inside one would never run"
    )]
    NestedInFamily { path: String },
    #[error(
        "{path}: a family directory holds `.toml` case files only; this one would \
         be skipped silently, and a dropped fixture is coverage the corpus does \
         not have"
    )]
    StrayFamilyFile { path: String },
}

/// What a family is for.
///
/// Not every joint fixture is a publish gate. `proration` is AC #61 — a
/// field-consumption contract shared with Subscriptions and Tariffs — and
/// blocks no publish at all. Stating the role explicitly is what separates
/// "gates nothing deliberately" from "someone forgot the gates list".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateRole {
    /// Blocks publish of the listed `modelKind`s through `FixtureGate`, **in
    /// this family's [`Variant`](crate::Variant)**.
    ///
    /// The variant is the second half of the sentence and it is not authored
    /// here: it is read off the family ([`crate::model::Family::variant`]). A
    /// `Publish` family therefore says "these kinds may not publish *this
    /// scenario* without me", which is what lets `supersession-continuity` gate
    /// the tiered kinds (D-22) without claiming to be their `modelKind` fixture.
    Publish,
    /// A joint conformance regression that blocks no publish.
    Conformance,
}

/// A family's `_family.toml` — what this family gates, and why it exists.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyMeta {
    pub family: Family,
    pub role: GateRole,
    /// The `modelKind`s whose publish this family gates. Empty exactly when the
    /// role is `Conformance`.
    #[serde(default)]
    pub gates: Vec<ModelKind>,
    pub provenance: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Corpus {
    pub cases: Vec<Case>,
    pub families: Vec<FamilyMeta>,
}

impl Corpus {
    /// The committed corpus, resolved relative to this crate's manifest.
    #[must_use]
    pub fn corpus_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus")
    }

    /// Loads every family declaration and case under `root`.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError`] if a directory cannot be read, a file does not
    /// parse, a directory name is not a known family, a file declares a
    /// different family than the directory it sits in, or a `.toml` other than
    /// `registry.toml` sits at the root.
    pub fn load(root: &Path) -> Result<Self, CorpusError> {
        let mut cases = Vec::new();
        let mut families = Vec::new();

        let entries = read_dir(root)?;

        // The root is the one place an authored case could exist, never run, and
        // produce no error: the walk below takes directories only, so a `.toml`
        // dropped here was silently skipped and the registry regenerated cleanly
        // without it — an authored rule that nothing checks and nothing reports.
        // Every other misfiling inside the tree is already fail-closed
        // (`Misfiled`, `UnknownFamilyDirectory`, and the deliberate two-pass
        // parse that rejects stray keys), so this was the only open edge.
        //
        // `registry.toml` is the generated gate file and is the one exception.
        for path in &entries {
            if path.is_dir() || path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            if path.file_name().is_some_and(|n| n == "registry.toml") {
                continue;
            }
            return Err(CorpusError::StrayRootFile {
                path: display(path),
            });
        }

        let mut dirs: Vec<PathBuf> = entries.into_iter().filter(|p| p.is_dir()).collect();
        dirs.sort();

        for dir in dirs {
            let dir_family = family_from_dir_name(&dir)?;

            // Everything in a family directory is claimed, not filtered: the walk
            // never descends, so a nested directory hides every case under it, and
            // a mistyped extension (`.tml`, `.toml.bak`) hides one. Filtering skips
            // both in silence — the same open edge `StrayRootFile` closes at the
            // root.
            let mut entries: Vec<PathBuf> = Vec::new();
            for path in read_dir(&dir)? {
                if path.is_dir() {
                    return Err(CorpusError::NestedInFamily {
                        path: display(&path),
                    });
                }
                if path.extension().is_none_or(|e| e != "toml") {
                    return Err(CorpusError::StrayFamilyFile {
                        path: display(&path),
                    });
                }
                entries.push(path);
            }
            entries.sort();

            for path in entries {
                let text = fs::read_to_string(&path).map_err(|source| CorpusError::Io {
                    path: display(&path),
                    source,
                })?;

                if path.file_name().is_some_and(|n| n == "_family.toml") {
                    let meta: FamilyMeta =
                        toml::from_str(&text).map_err(|source| CorpusError::Parse {
                            path: display(&path),
                            source,
                        })?;
                    check_filed(&path, meta.family, dir_family)?;
                    families.push(meta);
                } else {
                    let case = parse_case(&path, &text)?;
                    check_filed(&path, case.family(), dir_family)?;
                    cases.push(case);
                }
            }
        }

        Ok(Corpus { cases, families })
    }

    pub fn cases_for(&self, family: Family) -> impl Iterator<Item = &Case> {
        self.cases.iter().filter(move |c| c.family() == family)
    }
}

/// Reads `kind` first, then parses the whole file into the matching type.
///
/// Two passes on purpose: an internally tagged enum cannot carry
/// `deny_unknown_fields`, and rejecting stray keys is the property that keeps
/// the snapshot/runtime ownership boundary honest.
fn parse_case(path: &Path, text: &str) -> Result<Case, CorpusError> {
    let parse_err = |source: toml::de::Error| CorpusError::Parse {
        path: display(path),
        source,
    };

    let header: CaseHeader = toml::from_str(text).map_err(parse_err)?;

    Ok(match header.kind {
        CaseKind::Evaluation => {
            Case::Evaluation(Box::new(toml::from_str(text).map_err(parse_err)?))
        }
        CaseKind::Publish => Case::Publish(Box::new(toml::from_str(text).map_err(parse_err)?)),
    })
}

fn check_filed(path: &Path, declared: Family, directory: Family) -> Result<(), CorpusError> {
    if declared == directory {
        Ok(())
    } else {
        Err(CorpusError::Misfiled {
            path: display(path),
            declared,
            directory,
        })
    }
}

fn family_from_dir_name(dir: &Path) -> Result<Family, CorpusError> {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    // Reuse the serde mapping so directory names and the `family` field cannot
    // drift apart.
    toml::from_str::<FamilyName>(&format!("family = {name:?}"))
        .map(|f| f.family)
        .map_err(|_| CorpusError::UnknownFamilyDirectory { path: display(dir) })
}

#[derive(Deserialize)]
struct FamilyName {
    family: Family,
}

fn read_dir(path: &Path) -> Result<Vec<PathBuf>, CorpusError> {
    let iter = fs::read_dir(path).map_err(|source| CorpusError::Io {
        path: display(path),
        source,
    })?;

    let mut out = Vec::new();
    for entry in iter {
        let entry = entry.map_err(|source| CorpusError::Io {
            path: display(path),
            source,
        })?;
        out.push(entry.path());
    }
    Ok(out)
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
#[path = "corpus_tests.rs"]
mod corpus_tests;

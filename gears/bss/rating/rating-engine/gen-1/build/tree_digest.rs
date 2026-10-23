//! `engine_digest`: SHA-256 over a generation's frozen tree (rating ADR-0006, DESIGN §4.1).
//!
//! Shared by the build script, which computes the digest the crate is compiled with, and by the
//! digest tests, which recompute it over an edited copy of the tree. The frozen tree is the
//! manifest, `vendor.toml`, the build script and every file under `build/`, `src/`, `vendor/` (the
//! vendored copy and `LOCK.toml`, which also records the shared money crate's sources and the
//! registry pins) and `golden/` (the golden vectors and their outputs).
//! Tests are not semantics and stay outside; so do hidden files such as `.DS_Store`. A symbolic
//! link is refused: its target would lie outside the tree the digest names. Every root file and
//! directory above is required: a checkout that lacks one fails the build rather than compiling in
//! the digest of an incomplete tree.
//!
//! Each file contributes its path relative to the generation root (`/`-separated), a NUL, its
//! length as eight big-endian bytes and its content, in byte order of the paths, so a renamed,
//! moved, added or removed file changes the digest as an edit does.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The files at the root of a generation that belong to its frozen tree.
pub const ROOT_FILES: [&str; 3] = ["Cargo.toml", "build.rs", "vendor.toml"];
/// The directories of a generation whose whole content belongs to its frozen tree.
pub const FROZEN_DIRS: [&str; 4] = ["build", "src", "vendor", "golden"];

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Every file of the frozen tree under `root`, as `(relative path, absolute path)`, sorted.
///
/// # Errors
///
/// An I/O error while reading a directory of the tree.
pub fn frozen_files(root: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    for name in ROOT_FILES {
        let path = root.join(name);
        let kind = required(&path)?;
        if !kind.is_file() {
            return Err(invalid(&path, "is not a file"));
        }
        files.push((name.to_owned(), path));
    }
    for dir in FROZEN_DIRS {
        let path = root.join(dir);
        if !required(&path)?.is_dir() {
            return Err(invalid(&path, "is not a directory"));
        }
        collect(root, &path, &mut files)?;
    }
    files.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    Ok(files)
}

// @cpt-begin:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-digest
/// The lower-case hex SHA-256 of the frozen tree under `root`.
///
/// # Errors
///
/// An I/O error while reading the tree.
pub fn tree_digest(root: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    for (relative, path) in frozen_files(root)? {
        let content = fs::read(&path)?;
        hasher.update(relative.as_bytes());
        hasher.update([0_u8]);
        hasher.update((content.len() as u64).to_be_bytes());
        hasher.update(&content);
    }
    Ok(hasher
        .finalize()
        .iter()
        .flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0x0f)]])
        .map(char::from)
        .collect())
}

// @cpt-end:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-digest

/// Adds every file under `dir` to `files`, refusing symbolic links and skipping hidden entries.
fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
            return Err(invalid(
                &path,
                "has a name that is not UTF-8 in a frozen tree",
            ));
        };
        if name.starts_with('.') {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(invalid(&path, "is a symbolic link in a frozen tree"));
        }
        if kind.is_dir() {
            collect(root, &path, files)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            let relative = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.push((relative, path));
        }
    }
    Ok(())
}

/// The type of a required entry of the frozen tree, never following a symbolic link.
fn required(path: &Path) -> io::Result<fs::FileType> {
    let kind = fs::symlink_metadata(path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?
        .file_type();
    if kind.is_symlink() {
        return Err(invalid(path, "is a symbolic link in a frozen tree"));
    }
    Ok(kind)
}

/// An `InvalidData` error naming `path`.
fn invalid(path: &Path, what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} {what}", path.display()),
    )
}

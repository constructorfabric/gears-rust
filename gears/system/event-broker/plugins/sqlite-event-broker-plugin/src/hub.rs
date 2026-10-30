//! What every instance over one database file in this process shares
//! (`docs/DESIGN.md` §3.2): the heads they have committed, and the turn to
//! write.
//!
//! One process owns the file, so every append to it passes through some
//! instance here: an append made through one instance wakes the waiting
//! reads and watches of every other at once, with no polling.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};

use event_broker_sdk::backend::support::LocalHeads;

/// The file's shared state.
pub struct Hub {
    /// Heads committed to the file by any instance in this process.
    pub heads: LocalHeads,
    /// Held across every write transaction: writers queue here in order
    /// rather than spin in `SQLite`'s busy handler. Shared, so an append's
    /// detached commit (`append::finish`) holds it until the commit ends.
    pub writer: Arc<tokio::sync::Mutex<()>>,
    /// Held while an instance opens the file and migrates it, so two
    /// instances starting together do not race to create the schema.
    pub opening: tokio::sync::Mutex<()>,
}

static HUBS: LazyLock<Mutex<HashMap<PathBuf, Weak<Hub>>>> = LazyLock::new(Mutex::default);

/// The hub of the file at `path`, made on first use and kept while an
/// instance holds it.
///
/// Keyed by the file's canonical path once it exists, so a symlink to it and
/// the file share a hub; before that, by the canonical directory and the file
/// name, which is what the file's canonical path will be once it is created.
/// Touches the file system: call it off the async runtime.
///
/// # Errors
/// The directory does not exist or cannot be resolved.
pub fn of(path: &Path) -> io::Result<Arc<Hub>> {
    let key = key(path)?;
    let mut hubs = HUBS.lock().unwrap_or_else(PoisonError::into_inner);
    hubs.retain(|_, hub| hub.strong_count() > 0);
    if let Some(hub) = hubs.get(&key).and_then(Weak::upgrade) {
        return Ok(hub);
    }
    let hub = Arc::new(Hub {
        heads: LocalHeads::new(),
        writer: Arc::new(tokio::sync::Mutex::new(())),
        opening: tokio::sync::Mutex::new(()),
    });
    hubs.insert(key, Arc::downgrade(&hub));
    Ok(hub)
}

fn key(path: &Path) -> io::Result<PathBuf> {
    if let Ok(file) = path.canonicalize() {
        return Ok(file);
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.canonicalize()?,
        _ => std::env::current_dir()?,
    };
    Ok(directory.join(name))
}

//! The results file: the progress file of `copy` and the only link between a
//! row and its new `value_version`.
//!
//! JSON Lines. The first line is a header
//! (`{"format":"credstore-value-migration","version":1,"created_at":...}`),
//! every further line is one [`Entry`]. A secret value or a fingerprint is
//! never written. Every entry is appended, flushed and fsynced before the next
//! row is touched.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::MigrationError;

/// Value of the header's `format` field.
pub const FORMAT: &str = "credstore-value-migration";
/// Value of the header's `version` field.
pub const VERSION: u32 = 1;

/// What `copy` did with one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Fingerprint verified, value copied to the new store.
    Copied,
    /// The row carried no fingerprint (out-of-band seeded; the shipped gear
    /// served such rows on trust): value copied, but never verified.
    Unverified,
    /// The legacy store held no value for an `active` row. Nothing copied.
    Missing,
    /// The value failed the fingerprint check. Not copied.
    FpMismatch,
    /// `fp_key_id` names a fence key other than the one the shipped gear
    /// used, so the fingerprint cannot be verified. Not copied.
    UnknownFenceKey,
    /// The row was `provisioning` or `deprovisioning`: nothing to copy,
    /// the legacy entry (if any) is only listed for cleanup.
    Unfinished,
}

impl Outcome {
    /// Stable lower-case name, as written to the results file and the log.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Copied => "copied",
            Self::Unverified => "unverified",
            Self::Missing => "missing",
            Self::FpMismatch => "fp_mismatch",
            Self::UnknownFenceKey => "unknown_fence_key",
            Self::Unfinished => "unfinished",
        }
    }

    /// Whether the outcome carries a `value_version` (the value was copied).
    #[must_use]
    pub fn is_copied(self) -> bool {
        matches!(self, Self::Copied | Self::Unverified)
    }

    /// Whether the row ends up without a value (`activate` suppresses it).
    #[must_use]
    pub fn is_loss(self) -> bool {
        matches!(
            self,
            Self::Missing | Self::FpMismatch | Self::UnknownFenceKey
        )
    }
}

/// One row's line in the results file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Row id, which is also the new store's `record_id`.
    pub id: Uuid,
    /// Tenant of the row.
    pub tenant_id: Uuid,
    /// Reference of the row (the legacy address).
    pub reference: String,
    /// Sharing code of the row: `1` private, `2`/`3` non-private.
    pub sharing: i16,
    /// Owner of the row; present only for private rows, where it is part of
    /// the legacy address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<Uuid>,
    /// Status of the row before the migration (`1`, `2` or `3`).
    pub status_before: i16,
    /// What `copy` did.
    pub outcome: Outcome,
    /// The version the new store returned (copied outcomes only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Header {
    format: String,
    version: u32,
    created_at: String,
}

fn io_err(path: &Path, source: std::io::Error) -> MigrationError {
    MigrationError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn bad(path: &Path, reason: impl Into<String>) -> MigrationError {
    MigrationError::ResultsFile {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

/// Outcome of parsing a results file.
struct Parsed {
    header_seen: bool,
    entries: Vec<Entry>,
    /// Bytes of the file that hold complete, valid content.
    valid_len: usize,
    /// The last valid line lacks its terminating newline.
    needs_newline: bool,
}

fn parse(path: &Path, text: &str) -> Result<Parsed, MigrationError> {
    let mut parsed = Parsed {
        header_seen: false,
        entries: Vec::new(),
        valid_len: 0,
        needs_newline: false,
    };
    let mut offset = 0usize;
    for segment in text.split_inclusive('\n') {
        let complete = segment.ends_with('\n');
        let line = segment.trim();
        offset += segment.len();
        if line.is_empty() {
            parsed.valid_len = offset;
            continue;
        }
        match parse_line(path, line, parsed.header_seen) {
            Ok(Line::Header) => parsed.header_seen = true,
            Ok(Line::Entry(entry)) => parsed.entries.push(entry),
            // A torn final line is what a crash mid-write leaves behind.
            Err(_) if !complete => continue,
            Err(e) => return Err(e),
        }
        parsed.valid_len = offset;
        parsed.needs_newline = !complete;
    }
    Ok(parsed)
}

enum Line {
    Header,
    Entry(Entry),
}

fn parse_line(path: &Path, line: &str, header_seen: bool) -> Result<Line, MigrationError> {
    if header_seen {
        let entry =
            serde_json::from_str(line).map_err(|e| bad(path, format!("invalid line: {e}")))?;
        return Ok(Line::Entry(entry));
    }
    let header: Header =
        serde_json::from_str(line).map_err(|e| bad(path, format!("invalid header: {e}")))?;
    if header.format != FORMAT || header.version != VERSION {
        return Err(bad(
            path,
            format!(
                "unsupported header: format {}, version {}",
                header.format, header.version
            ),
        ));
    }
    Ok(Line::Header)
}

/// Reads a results file without opening it for writing.
///
/// Returns `None` when the file does not exist.
///
/// # Errors
///
/// [`MigrationError::Io`] or [`MigrationError::ResultsFile`] when the file
/// cannot be read or is not a results file.
pub fn read(path: &Path) -> Result<Option<Vec<Entry>>, MigrationError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(path, e)),
    };
    let parsed = parse(path, &text)?;
    if !parsed.header_seen && !text.trim().is_empty() {
        return Err(bad(path, "missing header"));
    }
    Ok(Some(parsed.entries))
}

/// The results file opened for appending.
#[derive(Debug)]
pub struct ResultsWriter {
    path: PathBuf,
    file: std::fs::File,
    ids: HashSet<Uuid>,
}

impl ResultsWriter {
    /// Opens the file, creating it with a header when it does not exist, and
    /// returns the entries already in it. A torn final line (a crash
    /// mid-write) is discarded.
    ///
    /// # Errors
    ///
    /// [`MigrationError::Io`] or [`MigrationError::ResultsFile`].
    pub fn open(path: &Path) -> Result<(Self, Vec<Entry>), MigrationError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| io_err(path, e))?;
        let text = std::fs::read_to_string(path).map_err(|e| io_err(path, e))?;
        let parsed = parse(path, &text)?;
        let valid_len = u64::try_from(parsed.valid_len).map_err(|e| bad(path, e.to_string()))?;
        file.set_len(valid_len).map_err(|e| io_err(path, e))?;
        file.seek(SeekFrom::End(0)).map_err(|e| io_err(path, e))?;
        let mut writer = Self {
            path: path.to_path_buf(),
            file,
            ids: parsed.entries.iter().map(|e| e.id).collect(),
        };
        if !parsed.header_seen {
            let header = Header {
                format: FORMAT.to_owned(),
                version: VERSION,
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            let line = serde_json::to_string(&header).map_err(|e| bad(path, e.to_string()))?;
            writer.write_line(&line)?;
            sync_parent(path);
        } else if parsed.needs_newline {
            writer.write_line("")?;
        }
        Ok((writer, parsed.entries))
    }

    /// Whether an entry for `id` is already in the file.
    #[must_use]
    pub fn contains(&self, id: Uuid) -> bool {
        self.ids.contains(&id)
    }

    /// Appends one entry, flushes and fsyncs it.
    ///
    /// # Errors
    ///
    /// [`MigrationError::Io`] when the write or the sync fails.
    pub fn append(&mut self, entry: &Entry) -> Result<(), MigrationError> {
        let line = serde_json::to_string(entry).map_err(|e| bad(&self.path, e.to_string()))?;
        self.write_line(&line)?;
        self.ids.insert(entry.id);
        Ok(())
    }

    fn write_line(&mut self, line: &str) -> Result<(), MigrationError> {
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        self.file
            .write_all(buf.as_bytes())
            .and_then(|()| self.file.flush())
            .and_then(|()| self.file.sync_all())
            .map_err(|e| io_err(&self.path, e))
    }
}

/// Best effort: make the file's directory entry durable.
fn sync_parent(path: &Path) {
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(d) = std::fs::File::open(dir) {
            d.sync_all().ok();
        }
    }
}

//! `SQLite` PRAGMA parameter handling with typed enums.

use std::collections::HashMap;

/// `SQLite` journal mode options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalMode {
    Delete,
    Wal,
    Memory,
    Truncate,
    Persist,
    Off,
}

impl JournalMode {
    /// Parse from string (case-insensitive).
    fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "DELETE" => Some(JournalMode::Delete),
            "WAL" => Some(JournalMode::Wal),
            "MEMORY" => Some(JournalMode::Memory),
            "TRUNCATE" => Some(JournalMode::Truncate),
            "PERSIST" => Some(JournalMode::Persist),
            "OFF" => Some(JournalMode::Off),
            _ => None,
        }
    }
}

/// `SQLite` synchronous mode options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncMode {
    Off,
    Normal,
    Full,
    Extra,
}

impl SyncMode {
    /// Parse from string (case-insensitive).
    fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "OFF" => Some(SyncMode::Off),
            "NORMAL" => Some(SyncMode::Normal),
            "FULL" => Some(SyncMode::Full),
            "EXTRA" => Some(SyncMode::Extra),
            _ => None,
        }
    }
}

/// Parsed `SQLite` PRAGMA parameters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pragmas {
    pub journal_mode: Option<JournalMode>,
    pub synchronous: Option<SyncMode>,
    pub busy_timeout_ms: Option<i64>,
    /// Compatibility: support legacy `wal=true|false|1|0`
    pub wal_toggle: Option<bool>,
}

impl Pragmas {
    /// Parse PRAGMA parameters from a key-value map.
    pub(crate) fn from_pairs(pairs: &HashMap<String, String>) -> Self {
        let mut pragmas = Pragmas::default();

        for (key, value) in pairs {
            match key.to_lowercase().as_str() {
                "journal_mode" => {
                    pragmas.journal_mode = Self::parse_journal_mode(value);
                }
                "synchronous" => {
                    pragmas.synchronous = Self::parse_synchronous(value);
                }
                "busy_timeout" => {
                    pragmas.busy_timeout_ms = Self::parse_busy_timeout(value);
                }
                "wal" => {
                    pragmas.wal_toggle = Self::parse_wal_toggle(value);
                }
                _ => {
                    tracing::debug!("Unknown SQLite PRAGMA parameter: {}", key);
                }
            }
        }

        pragmas
    }

    /// Parse `journal_mode` PRAGMA value.
    fn parse_journal_mode(value: &str) -> Option<JournalMode> {
        if let Some(mode) = JournalMode::from_str(value) {
            Some(mode)
        } else {
            tracing::warn!("Invalid 'journal_mode' PRAGMA value '{}', ignoring", value);
            None
        }
    }

    /// Parse synchronous PRAGMA value.
    fn parse_synchronous(value: &str) -> Option<SyncMode> {
        if let Some(mode) = SyncMode::from_str(value) {
            Some(mode)
        } else {
            tracing::warn!("Invalid 'synchronous' PRAGMA value '{}', ignoring", value);
            None
        }
    }

    /// Parse `busy_timeout` PRAGMA value.
    fn parse_busy_timeout(value: &str) -> Option<i64> {
        match value.parse::<i64>() {
            Ok(timeout) if timeout >= 0 => Some(timeout),
            _ => {
                tracing::warn!("Invalid 'busy_timeout' PRAGMA value '{}', ignoring", value);
                None
            }
        }
    }

    /// Parse wal PRAGMA value (legacy compatibility).
    fn parse_wal_toggle(value: &str) -> Option<bool> {
        match value.to_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => {
                tracing::warn!("Invalid 'wal' PRAGMA value '{}', ignoring", value);
                None
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "pragmas_tests.rs"]
mod tests;

//! Path classifier — maps a repo-relative path to a `Location` bucket.
//!
//! Order of precedence (matches the Python orchestrator):
//! 1. `*.md` files                     → "doc"
//! 2. any component named `docs`       → "doc"
//! 3. any component `plugins` or `*-plugin` → "plugin"
//! 4. any component ending with `-sdk` → "sdk"
//! 5. recognised extensions            → "main"
//! 6. everything else                  → "other"

use std::path::Path;

const RECOGNISED_EXTS: &[&str] = &[".rs", ".toml", ".json", ".yaml", ".yml"];

/// Classifies a module-relative path as `sdk`, `main`, `plugin`, `doc` or `other`.
pub fn classify_location(rel: &Path) -> &'static str {
    let ext = rel
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    if ext.as_deref() == Some("md") {
        return "doc";
    }

    let mut saw_plugins_dir = false;
    let mut saw_plugin_segment = false;
    let mut saw_sdk_segment = false;

    for comp in rel.components() {
        let s = match comp.as_os_str().to_str() {
            Some(s) => s,
            None => continue,
        };
        if s == "docs" {
            return "doc";
        }
        if s == "plugins" {
            saw_plugins_dir = true;
        } else if s.ends_with("-plugin") {
            saw_plugin_segment = true;
        }
        if s.ends_with("-sdk") {
            saw_sdk_segment = true;
        }
    }

    if saw_plugins_dir || saw_plugin_segment {
        return "plugin";
    }
    if saw_sdk_segment {
        return "sdk";
    }

    if let Some(e) = ext {
        let dotted = format!(".{e}");
        if RECOGNISED_EXTS.contains(&dotted.as_str()) {
            return "main";
        }
    }
    "other"
}

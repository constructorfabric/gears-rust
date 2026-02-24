//! GTS Documentation Validator — local wrapper (DE0903)
//!
//! Thin CLI wrapper around `gts_validator` library crate.
//! All core validation logic lives in `gts-validator` (from `gts-rust`).
//!
//! # Usage
//!
//! ```bash
//! # Validate docs with vendor filter
//! gts-docs-validator --vendor x docs modules libs examples
//!
//! # With exclusions
//! gts-docs-validator --vendor x --exclude "target/*" --exclude "docs/api/*" .
//!
//! # JSON output
//! gts-docs-validator --vendor x --json docs
//! ```

// CLI tools are expected to print to stdout/stderr
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::exit,
    clippy::expect_used
)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use gts_validator::output;
use gts_validator::{DiscoveryMode, FsSourceConfig, ValidationConfig, VendorPolicy};

/// GTS Documentation Validator (DE0903)
///
/// Validates GTS identifiers in .md, .json, and .yaml files.
/// Ensures all GTS IDs follow the correct format and optionally validates
/// that they use a specific vendor.
#[derive(Parser, Debug)]
#[command(name = "gts-docs-validator")]
#[command(author, version, about, long_about = None)]
#[allow(clippy::struct_excessive_bools)]
struct Cli {
    /// Paths to scan (files or directories)
    /// Defaults to: docs, modules, libs, examples
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Expected vendor for all GTS IDs (validates vendor matches)
    /// Example: --vendor x ensures all IDs use vendor "x"
    ///
    /// Note: Example vendors are always tolerated: acme, globex, example, demo, test, sample, tutorial
    #[arg(long)]
    vendor: Option<String>,

    /// Exclude patterns (can be specified multiple times)
    /// Supports glob patterns. Example: --exclude "target/*" --exclude "docs/api/*"
    #[arg(long, short = 'e', action = clap::ArgAction::Append)]
    exclude: Vec<String>,

    /// Output results as JSON
    #[arg(long)]
    json: bool,

    /// Show verbose output including file scanning progress
    #[arg(long, short = 'v')]
    verbose: bool,

    /// Maximum file size in bytes (default: 10 MB)
    #[arg(long, default_value = "10485760")]
    max_file_size: u64,

    /// Scan JSON/YAML object keys for GTS identifiers (default: off)
    #[arg(long)]
    scan_keys: bool,

    /// Strict mode: uses a heuristic regex to catch ALL gts.* strings including malformed IDs.
    /// When disabled (default), only well-formed GTS patterns are matched (fewer false positives).
    #[arg(long)]
    strict: bool,

    /// Skip tokens: if any of these strings appear before a GTS candidate on the
    /// same line, validation is skipped for that candidate (case-insensitive).
    /// Example: --skip-token "**given**" to skip BDD-style bold formatting.
    #[arg(long = "skip-token", action = clap::ArgAction::Append)]
    skip_tokens: Vec<String>,
}

/// Default directories to scan if none specified
const DEFAULT_SCAN_DIRS: &[&str] = &["docs", "modules", "libs", "examples"];

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Determine paths to scan
    let paths: Vec<PathBuf> = if cli.paths.is_empty() {
        DEFAULT_SCAN_DIRS
            .iter()
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect()
    } else {
        cli.paths
    };

    if paths.is_empty() {
        eprintln!("No existing paths to scan. Provide paths explicitly.");
        return ExitCode::FAILURE;
    }

    let mut fs_config = FsSourceConfig::default();
    fs_config.paths = paths;
    fs_config.exclude = cli.exclude;
    fs_config.max_file_size = cli.max_file_size;

    let mut validation_config = ValidationConfig::default();
    validation_config.vendor_policy = match cli.vendor {
        Some(v) => VendorPolicy::MustMatch(v),
        None => VendorPolicy::Any,
    };
    validation_config.scan_keys = cli.scan_keys;
    validation_config.discovery_mode = if cli.strict {
        DiscoveryMode::Heuristic
    } else {
        DiscoveryMode::StrictSpecOnly
    };
    validation_config.skip_tokens = cli.skip_tokens;

    if cli.verbose {
        let path_list: Vec<String> = fs_config
            .paths
            .iter()
            .map(|p: &PathBuf| p.display().to_string())
            .collect();
        eprintln!("Scanning paths: {}", path_list.join(", "));
        if let VendorPolicy::MustMatch(ref vendor) = validation_config.vendor_policy {
            eprintln!("Expected vendor: {vendor}");
        }
    }

    let report = match gts_validator::validate_fs(&fs_config, &validation_config) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if cli.verbose {
        eprintln!("Scanned {} files", report.scanned_files);
    }

    // Output results using shared formatters
    let mut stdout = std::io::stdout();
    let result = if cli.json {
        output::write_json(&report, &mut stdout)
    } else {
        output::write_human(&report, &mut stdout)
    };

    if let Err(e) = result {
        eprintln!("Error writing output: {e}");
        return ExitCode::FAILURE;
    }

    if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

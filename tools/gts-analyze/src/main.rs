//! GTS module analyzer — orchestrator + CLI.
//!
//! Scans a single gear directory for the Global Type System (GTS):
//! types defined here, instances declared here, and all GTS identifier references
//! across `.rs` / `.md` / `.json` / `.toml` / `.yaml` files. Output: markdown or JSON.

#![forbid(unsafe_code)]

mod classify;
mod model;
/// Rendering of the scan report as Markdown or JSON.
mod render;
/// Per-file scanners that extract GTS types, instances and references.
mod scan;
mod walk;

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;

use crate::classify::classify_location;
use crate::model::Report;
use crate::scan::{json as scan_json, markdown as scan_md, rust as scan_rust};
use crate::walk::Walker;

#[derive(Parser, Debug)]
#[command(
    name = "gts-analyze",
    version,
    about = "Analyze GTS (Global Type System) usage in a single Gear."
)]
struct Cli {
    /// Gear directory to scan.
    module_path: PathBuf,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Md)]
    format: Format,

    /// Write report to FILE instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Include Rust test files: integration tests (`tests/`), benches, `*_test(s).rs`,
    /// and items annotated `#[test]` / `#[bench]` / positive `#[cfg(test)]`.
    #[arg(long)]
    include_tests: bool,

    /// Skip every `*.md` file and every file under any `docs/` directory.
    /// Type definitions sourced from `docs/schemas/*.schema.json` will not appear.
    #[arg(long)]
    skip_docs: bool,

    /// Expand reference listings to per-line `file:line` + context.
    /// Default collapses each location to one row per file with a hit count.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum Format {
    Md,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let root = fs::canonicalize(&cli.module_path)
        .with_context(|| format!("cannot resolve {}", cli.module_path.display()))?;
    if !root.is_dir() {
        anyhow::bail!("not a directory: {}", root.display());
    }

    let mut rep = Report {
        module_root: root.display().to_string(),
        include_tests: cli.include_tests,
        skip_docs: cli.skip_docs,
        verbose: cli.verbose,
        ..Default::default()
    };

    let walker = Walker::new(root.clone(), cli.include_tests, cli.skip_docs);
    for (abs, rel) in walker.iter() {
        let text = match fs::read_to_string(&abs) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let ext = abs
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();

        *rep.file_counts.entry(ext.clone()).or_insert(0) += 1;
        *rep.location_counts
            .entry(classify_location(&rel).to_string())
            .or_insert(0) += 1;

        match ext.as_str() {
            "rs" => {
                let mut sub = scan_rust::scan_file(&rel, &text, cli.include_tests);
                rep.types.append(&mut sub.types);
                rep.instances.append(&mut sub.instances);
                rep.references.append(&mut sub.references);
            }
            "json" => {
                scan_json::scan_file(&rel, &text, &mut rep.types, &mut rep.references);
            }
            other => {
                scan_md::scan_file(&rel, &text, other, &mut rep.references);
            }
        }
    }

    let payload = match cli.format {
        Format::Md => render::md::render(&rep),
        Format::Json => render::json::render(&rep),
    };

    if let Some(path) = cli.out {
        fs::write(&path, &payload).with_context(|| format!("writing {}", path.display()))?;
        eprintln!("report written to {}", path.display());
    } else {
        print!("{payload}");
    }
    Ok(())
}

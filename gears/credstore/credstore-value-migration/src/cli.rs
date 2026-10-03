//! Command-line entry point operators link with their own store adapters.
//!
//! ```text
//! migrate --results results.jsonl --database-url postgres://... copy            # dry run
//! migrate --results results.jsonl --database-url postgres://... copy --apply
//! migrate --results results.jsonl --database-url postgres://... activate --apply
//! migrate --results results.jsonl cleanup --apply [--include-fence-key]
//! ```
//!
//! Every stage defaults to a dry run and writes nothing until `--apply`.
//!
//! Exit codes: `0` success; `1` the stage aborted (nothing proceeds); `2` the
//! stage ran but needs a conscious decision: `copy` found rows that end up
//! without a value (`missing`, `fp_mismatch`, `unknown_fence_key`), or
//! `activate` met rows it must not touch. Inspect the report, then re-run the
//! same command with `--accept-losses` (`copy`) to proceed; the re-run
//! resumes from the results file, so it is quick.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use credstore_sdk::CredStorePluginClientV2;
use sea_orm::Database;

use crate::Mode;
use crate::activate::{ActivateReport, activate};
use crate::cleanup::{CleanupReport, cleanup};
use crate::copy::{CopyReport, RowRef, copy};
use crate::error::MigrationError;
use crate::legacy::LegacyValueStore;

const EXIT_ABORTED: u8 = 1;
const EXIT_NEEDS_DECISION: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "credstore-value-migration",
    about = "Moves CredStore values into the immutable-versions store (one-off, stop-the-world)"
)]
struct Cli {
    /// Database of the credstore gear (postgres:// or sqlite://). Not needed
    /// by `cleanup`.
    #[arg(
        long,
        env = "CREDSTORE_MIGRATION_DATABASE_URL",
        global = true,
        hide_env_values = true
    )]
    database_url: Option<String>,
    /// Results file (JSON Lines): the progress file of `copy`, the input of
    /// `activate` and `cleanup`. Keep it until the migration is verified.
    #[arg(long, global = true, default_value = "credstore-value-migration.jsonl")]
    results: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Before m0002: copy every active value into the new store.
    Copy {
        /// Perform the copy (default: dry run, read-only).
        #[arg(long)]
        apply: bool,
        /// Exit 0 although some rows end up without a value.
        #[arg(long)]
        accept_losses: bool,
    },
    /// After m0002: point the migrated rows at their new versions.
    Activate {
        /// Perform the update (default: dry run).
        #[arg(long)]
        apply: bool,
    },
    /// After testing: delete the superseded entries of the old store.
    Cleanup {
        /// Perform the deletes (default: dry run).
        #[arg(long)]
        apply: bool,
        /// Also delete the old fence key (last).
        #[arg(long)]
        include_fence_key: bool,
    },
}

fn mode(apply: bool) -> Mode {
    if apply { Mode::Apply } else { Mode::DryRun }
}

/// Runs the tool with `std::env::args`.
///
/// # Errors
///
/// Only when the output cannot be written; stage failures are reported on
/// stderr and returned as exit code `1`.
pub async fn run_cli(
    legacy: Arc<dyn LegacyValueStore>,
    target: Arc<dyn CredStorePluginClientV2>,
) -> anyhow::Result<ExitCode> {
    run_cli_from(std::env::args_os(), Some(legacy), Some(target)).await
}

/// Like [`run_cli`] with explicit arguments (the first is the program name)
/// and optional stores: `activate` needs neither, `cleanup` only `legacy`,
/// `copy` both.
///
/// # Errors
///
/// Only when the output cannot be written.
pub async fn run_cli_from<I, T>(
    args: I,
    legacy: Option<Arc<dyn LegacyValueStore>>,
    target: Option<Arc<dyn CredStorePluginClientV2>>,
) -> anyhow::Result<ExitCode>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            e.print()?;
            return Ok(ExitCode::from(if e.use_stderr() {
                EXIT_NEEDS_DECISION
            } else {
                0
            }));
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .ok();
    match execute(&cli, legacy, target).await {
        Ok(code) => Ok(code),
        Err(e) => {
            writeln!(std::io::stderr(), "ABORTED: {e}")?;
            Ok(ExitCode::from(EXIT_ABORTED))
        }
    }
}

fn need<T>(store: Option<T>, what: &str) -> Result<T, MigrationError> {
    store.ok_or_else(|| {
        MigrationError::WrongSchema(format!("this binary was built without the {what}"))
    })
}

async fn connect(
    cli: &Cli,
    out: &mut std::io::Stdout,
) -> anyhow::Result<sea_orm::DatabaseConnection> {
    let url = cli.database_url.as_deref().ok_or_else(|| {
        anyhow::anyhow!("--database-url (or CREDSTORE_MIGRATION_DATABASE_URL) is required")
    })?;
    writeln!(
        out,
        "database: {}",
        toolkit_db::redact_credentials_in_dsn(Some(url))
    )?;
    Ok(Database::connect(url).await?)
}

async fn execute(
    cli: &Cli,
    legacy: Option<Arc<dyn LegacyValueStore>>,
    target: Option<Arc<dyn CredStorePluginClientV2>>,
) -> anyhow::Result<ExitCode> {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    match cli.command {
        Command::Copy {
            apply,
            accept_losses,
        } => {
            let legacy = need(legacy, "legacy store adapter")?;
            let target = need(target, "new store plugin")?;
            let db = connect(cli, &mut out).await?;
            let m = mode(apply);
            let report = copy(&db, legacy.as_ref(), target.as_ref(), &cli.results, m).await?;
            print_copy(&mut out, &report, m)?;
            if report.has_losses() && !accept_losses {
                writeln!(
                    err,
                    "Some rows end up without a value. Inspect the list above; to proceed anyway \
                     run the same command again with --accept-losses."
                )?;
                return Ok(ExitCode::from(EXIT_NEEDS_DECISION));
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Activate { apply } => {
            let db = connect(cli, &mut out).await?;
            let m = mode(apply);
            let report = activate(&db, &cli.results, m).await?;
            print_activate(&mut out, &report, m)?;
            if report.unexpected.is_empty() {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(EXIT_NEEDS_DECISION))
            }
        }
        Command::Cleanup {
            apply,
            include_fence_key,
        } => {
            let legacy = need(legacy, "legacy store adapter")?;
            let m = mode(apply);
            let report = cleanup(legacy.as_ref(), &cli.results, m, include_fence_key).await?;
            print_cleanup(&mut out, &report, m)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn banner(mode: Mode) -> &'static str {
    match mode {
        Mode::DryRun => "DRY RUN (nothing was changed; re-run with --apply)",
        Mode::Apply => "APPLIED",
    }
}

fn print_rows(out: &mut dyn Write, title: &str, rows: &[RowRef]) -> std::io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    writeln!(out, "{title}: {}", rows.len())?;
    for r in rows {
        writeln!(
            out,
            "  id={} tenant={} reference={}",
            r.id, r.tenant_id, r.reference
        )?;
    }
    Ok(())
}

fn print_copy(out: &mut dyn Write, r: &CopyReport, mode: Mode) -> std::io::Result<()> {
    writeln!(out, "copy: {}", banner(mode))?;
    writeln!(
        out,
        "rows: {} (already in the results file: {})",
        r.total_rows, r.resumed
    )?;
    writeln!(
        out,
        "fence key in the legacy store: {}",
        r.fence_key_present
    )?;
    writeln!(out, "copied (fingerprint verified): {}", r.copied)?;
    writeln!(
        out,
        "copied unverified (no fingerprint, served on trust before): {}",
        r.unverified
    )?;
    print_rows(out, "MISSING (no value in the legacy store)", &r.missing)?;
    print_rows(out, "FP_MISMATCH (not copied)", &r.fp_mismatch)?;
    print_rows(out, "UNKNOWN_FENCE_KEY (not copied)", &r.unknown_fence_key)?;
    print_rows(
        out,
        "unfinished rows (status 1/3, listed for cleanup)",
        &r.unfinished,
    )?;
    if !r.type_divergent.is_empty() {
        writeln!(
            out,
            "type-divergent pairs: {} (they keep working; a NEW private override with a \
             differing type is rejected after the cutover; divergence across tenants cannot be \
             computed without the tenant hierarchy)",
            r.type_divergent.len()
        )?;
        for d in &r.type_divergent {
            writeln!(
                out,
                "  tenant={} reference={} private={} ({}) non-private={} ({})",
                d.tenant_id,
                d.reference,
                d.private_row,
                d.private_type,
                d.nonprivate_row,
                d.nonprivate_type
            )?;
        }
    }
    Ok(())
}

fn print_activate(out: &mut dyn Write, r: &ActivateReport, mode: Mode) -> std::io::Result<()> {
    writeln!(out, "activate: {}", banner(mode))?;
    writeln!(out, "promoted: {}", r.promoted)?;
    writeln!(out, "left declared with fallback = none: {}", r.suppressed)?;
    writeln!(out, "already done: {}", r.already_done)?;
    writeln!(out, "ignored unfinished entries: {}", r.ignored_unfinished)?;
    if !r.unknown_ids.is_empty() {
        writeln!(
            out,
            "unknown ids (row deleted in the meantime): {}",
            r.unknown_ids.len()
        )?;
        for id in &r.unknown_ids {
            writeln!(out, "  {id}")?;
        }
    }
    if !r.unexpected.is_empty() {
        writeln!(
            out,
            "UNEXPECTED state, left untouched: {}",
            r.unexpected.len()
        )?;
        for id in &r.unexpected {
            writeln!(out, "  {id}")?;
        }
    }
    Ok(())
}

fn print_cleanup(out: &mut dyn Write, r: &CleanupReport, mode: Mode) -> std::io::Result<()> {
    writeln!(out, "cleanup: {}", banner(mode))?;
    writeln!(out, "legacy entries deleted: {}", r.deleted)?;
    writeln!(
        out,
        "missing entries (nothing to delete): {}",
        r.nothing_to_delete
    )?;
    print_rows(out, "kept as evidence", &r.kept_as_evidence)?;
    writeln!(out, "fence key deleted: {}", r.fence_key_deleted)?;
    Ok(())
}

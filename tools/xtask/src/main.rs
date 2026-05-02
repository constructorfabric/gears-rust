use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    // codacy:ignore -- build helper CLI, no security context
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");

    match cmd {
        "split-debug" => split_debug(&args[1..]),
        "help" | "--help" | "-h" => {
            print_help();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command: {other}");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    eprintln!(
        "\
Usage: cargo xtask <COMMAND>

Commands:
  split-debug <NAME>  Split debug symbols out of a release binary
                      using platform-native tools.
                      NAME is the binary name (e.g. cf-server).
                      Resolved as target/release/<NAME>.
  help                  Show this message."
    );
}

// ---------------------------------------------------------------------------
// split-debug
// ---------------------------------------------------------------------------

fn split_debug(args: &[String]) -> ExitCode {
    let Some(name) = args.first().map(|s| s.as_str()) else {
        eprintln!("error: binary name required");
        eprintln!("usage: cargo xtask split-debug <NAME>");
        return ExitCode::FAILURE;
    };
    let bin_name = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let bin = target_dir().join("release").join(&bin_name);

    if !bin.exists() {
        eprintln!("binary not found: {}", bin.display());
        eprintln!("run `cargo build --release` first");
        return ExitCode::FAILURE;
    }

    eprintln!("binary: {}", bin.display());
    show_file_type(&bin);

    let ok = if cfg!(target_os = "macos") {
        split_dsym(&bin)
    } else if cfg!(target_os = "windows") {
        // MSVC builds produce a PDB next to the binary; GNU/MinGW builds
        // use DWARF and need objcopy instead.  Try PDB first, fall back.
        if has_pdb(&bin) {
            split_pdb(&bin)
        } else {
            eprintln!("no PDB found — assuming GNU/MinGW toolchain, using objcopy");
            split_dwarf(&bin)
        }
    } else {
        split_dwarf(&bin)
    };

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ---------------------------------------------------------------------------
// macOS: dsymutil + strip
// ---------------------------------------------------------------------------

fn split_dsym(bin: &Path) -> bool {
    let bin_s = bin.to_str().unwrap();
    let size_before = file_size(bin);

    eprintln!("extracting .dSYM bundle...");
    if !run(&["dsymutil", bin_s], bin.parent().unwrap()) {
        eprintln!("dsymutil failed — install Xcode command-line tools");
        return false;
    }

    eprintln!("stripping binary...");
    if !run(&["strip", "-u", "-r", bin_s], bin.parent().unwrap()) {
        eprintln!("strip failed");
        return false;
    }

    let size_after = file_size(bin);
    let dsym = bin.with_extension("dSYM");
    let dsym_size = dir_size(&dsym);
    eprintln!("done:");
    eprintln!(
        "  binary:  {} ({} -> {})",
        bin.display(),
        fmt_size(size_before),
        fmt_size(size_after)
    );
    eprintln!("  symbols: {} ({})", dsym.display(), fmt_size(dsym_size));
    show_file_type(bin);
    true
}

// ---------------------------------------------------------------------------
// Linux / Windows-GNU: objcopy split + strip + debuglink
// ---------------------------------------------------------------------------

fn split_dwarf(bin: &Path) -> bool {
    let bin_s = bin.to_str().unwrap();
    let dbg = bin.with_extension("debug");
    let dbg_s = dbg.to_str().unwrap();
    let parent = bin.parent().unwrap();

    eprintln!("extracting debug symbols...");
    if !run(&["objcopy", "--only-keep-debug", bin_s, dbg_s], parent) {
        eprintln!("objcopy not found — install binutils");
        return false;
    }

    eprintln!("stripping binary...");
    if !run(&["objcopy", "--strip-debug", bin_s], parent) {
        return false;
    }

    eprintln!("attaching debug link...");
    let link_arg = format!("--add-gnu-debuglink={dbg_s}");
    if !run(&["objcopy", &link_arg, bin_s], parent) {
        return false;
    }

    eprintln!("done: {} (stripped)", bin.display());
    eprintln!("      {} (debug symbols)", dbg.display());
    show_file_type(bin);
    true
}

// ---------------------------------------------------------------------------
// Windows MSVC: PDB is already separate — just verify it exists
// ---------------------------------------------------------------------------

/// Check whether a PDB file exists next to the binary (MSVC builds).
/// Rust/MSVC may name it with underscores even when the binary uses hyphens.
fn has_pdb(bin: &Path) -> bool {
    find_pdb(bin).is_some()
}

fn find_pdb(bin: &Path) -> Option<PathBuf> {
    let pdb = bin.with_extension("pdb");
    if pdb.exists() {
        return Some(pdb);
    }
    let alt = bin.file_stem().and_then(|s| s.to_str()).map(|stem| {
        let underscored = stem.replace('-', "_");
        bin.with_file_name(format!("{underscored}.pdb"))
    });
    alt.filter(|p| p.exists())
}

fn split_pdb(bin: &Path) -> bool {
    match find_pdb(bin) {
        Some(p) => {
            eprintln!("PDB already separate: {}", p.display());
            eprintln!("no stripping needed for MSVC builds");
            true
        }
        None => {
            eprintln!(
                "no PDB found next to {} — was this built with MSVC?",
                bin.display()
            );
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn target_dir() -> PathBuf {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output()
        .expect("failed to run cargo metadata");
    let json = String::from_utf8(output.stdout).expect("cargo metadata output is not UTF-8");
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json)
        && let Some(dir) = value["target_directory"].as_str()
    {
        return PathBuf::from(dir);
    }
    // Fallback: assume <workspace>/target
    let locate = Command::new(env!("CARGO"))
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .expect("failed to run cargo locate-project");
    let path = String::from_utf8(locate.stdout).expect("cargo locate-project output is not UTF-8");
    PathBuf::from(path.trim()).parent().unwrap().join("target")
}

fn file_size(p: &Path) -> u64 {
    fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn dir_size(p: &Path) -> u64 {
    if p.is_file() {
        return file_size(p);
    }
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(p) {
        for entry in entries.flatten() {
            total += dir_size(&entry.path());
        }
    }
    total
}

fn fmt_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn run(args: &[&str], cwd: &Path) -> bool {
    eprintln!("  + {}", args.join(" "));
    let status = Command::new(args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .status();
    match status {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("  failed to run `{}`: {e}", args[0]);
            false
        }
    }
}

fn run_tool(name: &str, args: &[&str]) {
    let mut all: Vec<&str> = vec![name];
    all.extend_from_slice(args);
    let _ = run(&all, Path::new("."));
}

fn show_file_type(path: &Path) {
    if cfg!(windows) {
        return;
    }
    let Some(s) = path.to_str() else {
        return;
    };
    run_tool("file", &[s]);
}

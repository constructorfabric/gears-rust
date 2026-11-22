//! Bundles libpdfium for the PDF parser.
//!
//! The library comes from bblanchon/pdfium-binaries, pinned to one Chromium
//! build and verified against a SHA-256 digest per target before anything is
//! extracted from it. Only the library file is taken from the archive; it is
//! embedded into the crate (`include_bytes!`) and written to a private
//! temporary directory at runtime, where `pdfium-render` loads it. No link
//! directives are emitted: the library is opened with `dlopen`.
//!
//! Inputs (each re-runs the script when it changes):
//! - `FILE_PARSER_PDFIUM_ARCHIVE`: a downloaded `pdfium-<target>.tgz` to use
//!   instead of the network (offline builds). Still verified.
//! - `FILE_PARSER_PDFIUM_LIB`: a prebuilt library to embed as is. Not
//!   verified; a warning says so.
//! - `FILE_PARSER_PDFIUM_CACHE_DIR`: where downloaded archives are kept
//!   (default: `OUT_DIR`), so worktrees and `cargo clean` can share them.
//! - `CARGO_NET_OFFLINE=true`: no download; the archive must be cached or
//!   given. Cargo does not pass `--offline` on to build scripts, so offline
//!   builds set this variable or `FILE_PARSER_PDFIUM_ARCHIVE`.
//! - `DOCS_RS`: nothing is bundled.
//!
//! Targets without a pinned archive, and docs.rs, build without a bundled
//! library (`cfg(pdfium_unbundled)`); PDF parsing then needs
//! `FILE_PARSER_PDFIUM_RUNTIME_LIB` at runtime.

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

// SHA-256 through the workspace crypto facade (architecture lint DE0708).
use toolkit_security::crypto::hash::Sha256;

/// The pinned pdfium build. `pdfium-render` must be built with the highest
/// `pdfium_NNNN` feature not above it (`pdfium_7881`): a newer feature asks for
/// symbols this build lacks and binding fails.
const PDFIUM_BUILD: &str = "7961";

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Largest library file accepted from an archive.
const MAX_LIBRARY_BYTES: u64 = 64 * 1024 * 1024;

struct Asset {
    /// Archive name in the release.
    name: &'static str,
    /// SHA-256 of the archive, from the release's published digests.
    sha256: &'static str,
    /// Path of the library inside the archive.
    member: &'static str,
}

/// The archive for a target, by `(target_os, target_arch, target_env)`.
fn asset(os: &str, arch: &str, target_env: &str) -> Option<Asset> {
    let (name, sha256, member) = match (os, arch, target_env) {
        ("linux", "x86_64", "gnu") => (
            "pdfium-linux-x64.tgz",
            "019665c8877d46fe65f625f80fd714ab07aac68554b0636acf2a2adf9288adb2",
            "lib/libpdfium.so",
        ),
        ("linux", "aarch64", "gnu") => (
            "pdfium-linux-arm64.tgz",
            "974107999784a438149605024475d42d80dd306799d90e1af5f6fa63f976455f",
            "lib/libpdfium.so",
        ),
        ("macos", "aarch64", _) => (
            "pdfium-mac-arm64.tgz",
            "1193a771e0bd934530afa3df73a0d44551d8f4078442e290054e6dd38ded960f",
            "lib/libpdfium.dylib",
        ),
        ("macos", "x86_64", _) => (
            "pdfium-mac-x64.tgz",
            "17f069d7012ab83898ad5eddebd139b240f05d7411c220775d507a0e3e285536",
            "lib/libpdfium.dylib",
        ),
        ("windows", "x86_64", _) => (
            "pdfium-win-x64.tgz",
            "88276459349b291c41f10422dad0210f007c04d919c8fa56472b6b7c6406adf4",
            "bin/pdfium.dll",
        ),
        ("windows", "aarch64", _) => (
            "pdfium-win-arm64.tgz",
            "9d8c50c65f129c3774b972cde09d3bea44478075f72faadfe9fc0c152e65d509",
            "bin/pdfium.dll",
        ),
        // musl: a statically linked binary cannot dlopen the shared library
        // the release ships, so musl builds are unbundled.
        _ => return None,
    };
    Some(Asset {
        name,
        sha256,
        member,
    })
}

fn main() {
    if let Err(e) = run() {
        panic!("file-parser: cannot bundle libpdfium: {e}");
    }
}

fn run() -> Result<(), String> {
    println!("cargo::rustc-check-cfg=cfg(pdfium_unbundled)");
    println!("cargo:rerun-if-changed=build.rs");
    for var in [
        "FILE_PARSER_PDFIUM_ARCHIVE",
        "FILE_PARSER_PDFIUM_LIB",
        "FILE_PARSER_PDFIUM_CACHE_DIR",
        "CARGO_NET_OFFLINE",
        "DOCS_RS",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    if env::var_os("DOCS_RS").is_some() {
        unbundled("docs.rs build");
        return Ok(());
    }
    if let Some(lib) = env::var_os("FILE_PARSER_PDFIUM_LIB") {
        let lib = PathBuf::from(lib);
        println!("cargo:rerun-if-changed={}", lib.display());
        println!(
            "cargo:warning=embedding {} without verification (FILE_PARSER_PDFIUM_LIB)",
            lib.display()
        );
        return embed(&lib);
    }

    let os = env_var("CARGO_CFG_TARGET_OS")?;
    let arch = env_var("CARGO_CFG_TARGET_ARCH")?;
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let Some(asset) = asset(&os, &arch, &target_env) else {
        unbundled(&format!("no pinned pdfium for {arch}-{os}-{target_env}"));
        return Ok(());
    };

    let out_dir = PathBuf::from(env_var("OUT_DIR")?);
    let archive = archive_bytes(&asset, &out_dir)?;
    let library = extract(&archive, asset.member)?;
    let dir = out_dir.join("pdfium");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file_name = Path::new(asset.member)
        .file_name()
        .ok_or("archive member has no file name")?;
    let path = dir.join(file_name);
    write_atomically(&path, &library)?;
    embed(&path)
}

fn env_var(name: &str) -> Result<String, String> {
    env::var(name).map_err(|_| format!("{name} is not set"))
}

fn unbundled(reason: &str) {
    println!(
        "cargo:warning=building file-parser without a bundled libpdfium ({reason}); \
         PDF parsing needs FILE_PARSER_PDFIUM_RUNTIME_LIB at runtime"
    );
    println!("cargo:rustc-cfg=pdfium_unbundled");
}

fn embed(path: &Path) -> Result<(), String> {
    let path = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "cargo:rustc-env=FILE_PARSER_PDFIUM_EMBED={}",
        path.display()
    );
    Ok(())
}

/// The verified archive: given, cached, or downloaded into the cache.
fn archive_bytes(asset: &Asset, out_dir: &Path) -> Result<Vec<u8>, String> {
    if let Some(given) = env::var_os("FILE_PARSER_PDFIUM_ARCHIVE") {
        let given = PathBuf::from(given);
        println!("cargo:rerun-if-changed={}", given.display());
        let bytes = fs::read(&given).map_err(|e| format!("{}: {e}", given.display()))?;
        verify(asset, &bytes).map_err(|e| format!("{}: {e}", given.display()))?;
        return Ok(bytes);
    }

    let cache = env::var_os("FILE_PARSER_PDFIUM_CACHE_DIR")
        .map_or_else(|| out_dir.to_path_buf(), PathBuf::from)
        .join(format!("pdfium-{PDFIUM_BUILD}"))
        .join(asset.sha256);
    let cached = cache.join(asset.name);
    if let Ok(bytes) = fs::read(&cached) {
        if verify(asset, &bytes).is_ok() {
            return Ok(bytes);
        }
        // A partial or tampered file: fetch it again.
        let _removed = fs::remove_file(&cached);
    }

    if env::var("CARGO_NET_OFFLINE").is_ok_and(|v| v == "true") {
        return Err(format!(
            "offline build and no cached {} in {}; download \
             https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/{PDFIUM_BUILD}/{} \
             and point FILE_PARSER_PDFIUM_ARCHIVE at it",
            asset.name,
            cache.display(),
            asset.name
        ));
    }
    fs::create_dir_all(&cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    // A name of its own, so builds sharing the cache do not write one file.
    let partial = cache.join(format!("{}.{}.part", asset.name, std::process::id()));
    let result = download(asset, &partial).and_then(|()| {
        let bytes = fs::read(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
        verify(asset, &bytes)?;
        Ok(bytes)
    });
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(e) => {
            let _removed = fs::remove_file(&partial);
            return Err(e);
        }
    };
    if let Err(e) = fs::rename(&partial, &cached) {
        let _removed = fs::remove_file(&partial);
        // Another build may have put a verified copy there first.
        let ours_is_there = fs::read(&cached).is_ok_and(|b| verify(asset, &b).is_ok());
        if !ours_is_there {
            return Err(format!("{}: {e}", cached.display()));
        }
    }
    Ok(bytes)
}

/// Downloads the archive with `curl`, which honours proxy settings and ships
/// with macOS and Windows 10+.
fn download(asset: &Asset, to: &Path) -> Result<(), String> {
    let url = format!(
        "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/{PDFIUM_BUILD}/{}",
        asset.name
    );
    let status = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--retry",
            "5",
            "--retry-delay",
            "2",
            "--connect-timeout",
            "30",
            "--max-time",
            "600",
            "--output",
        ])
        .arg(to)
        .arg(&url)
        .status()
        .map_err(|e| format!("cannot run curl to fetch {url}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "curl failed ({status}) fetching {url}; set FILE_PARSER_PDFIUM_ARCHIVE to a \
             downloaded copy for offline builds"
        ))
    }
}

fn verify(asset: &Asset, bytes: &[u8]) -> Result<(), String> {
    let digest = Sha256::digest(bytes);
    let actual: String = digest
        .iter()
        .flat_map(|b| [HEX[usize::from(b >> 4)], HEX[usize::from(b & 0xf)]])
        .map(char::from)
        .collect();
    if actual == asset.sha256 {
        Ok(())
    } else {
        Err(format!(
            "SHA-256 mismatch for {}: expected {}, got {actual}",
            asset.name, asset.sha256
        ))
    }
}

/// The bytes of `member` from a gzipped tar archive. Nothing else is
/// extracted, so the archive's other paths never reach the file system.
fn extract(archive: &[u8], member: &str) -> Result<Vec<u8>, String> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let entries = tar.entries().map_err(|e| format!("bad archive: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("bad archive: {e}"))?;
        let path = entry.path().map_err(|e| format!("bad archive: {e}"))?;
        let path = path.to_string_lossy();
        if path.trim_start_matches("./") != member {
            continue;
        }
        if entry.size() > MAX_LIBRARY_BYTES {
            return Err(format!("{member} is larger than {MAX_LIBRARY_BYTES} bytes"));
        }
        let mut bytes = Vec::new();
        entry
            .take(MAX_LIBRARY_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("cannot read {member}: {e}"))?;
        return Ok(bytes);
    }
    Err(format!("{member} not found in the archive"))
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let partial = path.with_extension(format!("{}.part", std::process::id()));
    fs::write(&partial, bytes).map_err(|e| format!("{}: {e}", partial.display()))?;
    fs::rename(&partial, path).map_err(|e| format!("{}: {e}", path.display()))
}

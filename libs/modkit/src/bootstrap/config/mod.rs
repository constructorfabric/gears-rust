//! Configuration module for modkit-bootstrap
//!
//! This module provides configuration types and utilities for both host and `OoP` modules.

mod dump;

use anyhow::{Context, Result, ensure};
// Use DB config types from modkit-db
pub use modkit_db::{DbConnConfig, GlobalDatabaseConfig, PoolCfg};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::Level;

use crate::ConfigProvider;
use crate::telemetry::OpenTelemetryConfig;
use url::Url;

/// Normalize a path to use forward slashes (for cross-platform YAML/DSN compatibility).
fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Error type for vendor configuration access.
#[derive(thiserror::Error, Debug)]
pub enum VendorConfigError {
    #[error("vendor '{vendor}' not found in configuration")]
    NotFound { vendor: String },
    #[error("invalid config for vendor '{vendor}': {source}")]
    InvalidConfig {
        vendor: String,
        #[source]
        source: serde_json::Error,
    },
}

// Re-export dump functions
pub use dump::{
    dump_effective_modules_config_json, dump_effective_modules_config_yaml, list_module_names,
    redact_dsn_password, render_effective_modules_config,
};

/// Small typed view to parse each module entry.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleConfig {
    #[serde(default)]
    pub database: Option<DbConnConfig>,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub runtime: Option<ModuleRuntime>,
    #[serde(default)] // Used by the CLI
    pub metadata: serde_json::Value,
}

/// Runtime configuration for a module (local vs out-of-process).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ModuleRuntime {
    #[serde(default, rename = "type")]
    pub mod_type: RuntimeKind,
    /// Execution configuration for `OoP` modules.
    #[serde(default)]
    pub execution: Option<ExecutionConfig>,
}

/// Execution configuration for out-of-process modules.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfig {
    /// Path to the executable. Supports absolute paths or `~` expansion.
    pub executable_path: String,
    /// Command-line arguments to pass to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory for the process (optional, defaults to current dir).
    #[serde(default)]
    pub working_directory: Option<String>,
    /// Environment variables to set for the process.
    #[serde(default)]
    pub environment: HashMap<String, String>,
}

/// Module runtime kind.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    #[default]
    Local,
    Oop,
}

/// Main application configuration with strongly-typed global sections
/// and a flexible per-module configuration bag.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Core server configuration.
    pub server: ServerConfig,
    /// New typed database configuration (optional).
    pub database: Option<GlobalDatabaseConfig>,
    /// Logging configuration
    #[serde(default = "default_logging_config")]
    pub logging: LoggingConfig,
    /// OpenTelemetry configuration (resource, tracing, metrics).
    #[serde(default)]
    pub opentelemetry: OpenTelemetryConfig,
    /// Directory containing per-module YAML files (optional).
    #[serde(default)]
    pub modules_dir: Option<String>,
    /// Per-module configuration bag: `module_name` → arbitrary JSON/YAML value.
    #[serde(default)]
    pub modules: HashMap<String, serde_json::Value>,
    /// Per-vendor configuration bag: `vendor_name` → arbitrary JSON/YAML value.
    /// Allows vendors to add their own typed configuration sections.
    #[serde(default)]
    pub vendor: VendorConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        let server = ServerConfig::default();
        Self {
            server,
            database: None,
            logging: default_logging_config(),
            opentelemetry: OpenTelemetryConfig::default(),
            modules_dir: None,
            modules: HashMap::new(),
            vendor: VendorConfig::new(),
        }
    }
}

impl ConfigProvider for AppConfig {
    fn get_module_config(&self, module_name: &str) -> Option<&serde_json::Value> {
        self.modules.get(module_name)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_server_name")]
    pub name: String,
    #[serde(default = "default_home_dir")]
    pub home_dir: PathBuf, // will be normalized to absolute path
}

fn default_server_name() -> String {
    "cyberfabric".to_owned()
}

fn default_home_dir() -> PathBuf {
    super::host::paths::default_home_dir().join(".cyberfabric")
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: default_server_name(),
            home_dir: default_home_dir(),
        }
    }
}

impl ServerConfig {
    fn normalize_home_dir_inplace(&mut self) -> Result<()> {
        self.home_dir = super::host::normalize_path(
            self.home_dir
                .to_str()
                .context("home directory configuration is not a valid path")?,
        )
        .context("home_dir normalization failed")?;

        std::fs::create_dir_all(&self.home_dir).context("Failed to create home_dir")?;

        Ok(())
    }
}

/// Console output format for the logging layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleFormat {
    /// Human-readable text output (default).
    #[default]
    Text,
    /// Structured JSON output (useful for container log collectors).
    Json,
}

/// Logging configuration - maps subsystem names to their logging settings.
/// Key "default" is the catch-all for logs that don't match explicit subsystems.
pub type LoggingConfig = HashMap<String, Section>;

/// Per-vendor configuration bag: vendor name → arbitrary JSON/YAML value.
/// Each vendor's section can be deserialized into a typed struct via
/// [`AppConfig::vendor_config`] or [`AppConfig::vendor_config_or_default`].
pub type VendorConfig = HashMap<String, serde_json::Value>;

// ================= Custom serde module for optional Level (supports "off") =================
mod optional_level_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use tracing::Level;

    #[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)]
    pub fn serialize<S>(level: &Option<Level>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match level {
            Some(l) => serializer.serialize_str(l.as_str()),
            None => serializer.serialize_str("off"),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Level>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "trace" => Ok(Some(Level::TRACE)),
            "debug" => Ok(Some(Level::DEBUG)),
            "info" => Ok(Some(Level::INFO)),
            "warn" => Ok(Some(Level::WARN)),
            "error" => Ok(Some(Level::ERROR)),
            "off" | "none" => Ok(None),
            _ => Err(serde::de::Error::custom(format!("invalid level: {s}"))),
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    pub fn default() -> Option<Level> {
        Some(Level::INFO)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SectionFile {
    pub file: String,
    #[serde(
        default = "optional_level_serde::default",
        with = "optional_level_serde"
    )]
    pub file_level: Option<Level>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Section {
    #[serde(default)]
    pub console_format: ConsoleFormat,
    #[serde(
        default = "optional_level_serde::default",
        with = "optional_level_serde"
    )]
    pub console_level: Option<Level>,
    #[serde(flatten)]
    pub section_file: Option<SectionFile>,
    pub max_age_days: Option<u32>, // Not implemented yet
    #[serde(default)]
    pub max_backups: Option<usize>, // How many files to keep
    #[serde(default)]
    pub max_size_mb: Option<u64>, // Max size of the file in MB
}

impl Section {
    #[must_use]
    pub fn file(&self) -> Option<&str> {
        self.section_file
            .as_ref()
            .map(|f| f.file.as_str())
            .filter(|s| !s.is_empty())
    }

    #[must_use]
    pub fn file_level(&self) -> Option<Level> {
        self.section_file.as_ref().and_then(|f| f.file_level)
    }
}

/// Create a default logging configuration.
#[must_use]
pub fn default_logging_config() -> LoggingConfig {
    let mut logging = HashMap::new();
    logging.insert(
        "default".to_owned(),
        Section {
            console_level: Some(Level::INFO),
            section_file: Some(SectionFile {
                file: "logs/cyberfabric.log".to_owned(),
                file_level: Some(Level::DEBUG),
            }),
            console_format: ConsoleFormat::default(),
            max_age_days: Some(7),
            max_backups: Some(3),
            max_size_mb: Some(100),
        },
    );
    logging
}

impl AppConfig {
    /// Load configuration with layered loading: defaults → YAML file → environment variables.
    /// Also normalizes `server.home_dir` into an absolute path and creates the directory.
    ///
    /// # Errors
    /// Returns an error if configuration loading or `home_dir` resolution fails.
    pub fn load_layered(config_path: &PathBuf) -> Result<Self> {
        use figment::{
            Figment,
            providers::{Env, Format, Serialized},
        };

        // For layered loading, start from AppConfig::default() which provides logging
        // defaults (via default_logging_config()); other optional sections (database,
        // tracing, modules_dir) remain None unless overridden by YAML/ENV.
        let figment = Figment::new()
            .merge(Serialized::defaults(AppConfig::default()))
            .merge(StrictYaml::file(config_path))
            // Example: APP__SERVER__PORT=8087 maps to server.port
            .merge(Env::prefixed("APP__").split("__"));

        let mut config: AppConfig = figment
            .extract()
            .with_context(|| "Failed to extract config from figment".to_owned())?;

        // Normalize + create home_dir immediately.
        config
            .server
            .normalize_home_dir_inplace()
            .context("Failed to resolve server.home_dir")?;

        // Merge module files if modules_dir is specified.
        if let Some(dir) = config.modules_dir.as_ref() {
            merge_module_files(&mut config.modules, dir)?;
        }

        Ok(config)
    }

    /// Load configuration from file or create with default values.
    /// Also normalizes `server.home_dir` into an absolute path and creates the directory.
    ///
    /// # Errors
    /// Returns an error if configuration loading or `home_dir` resolution fails.
    pub fn load_or_default(config_path: Option<&PathBuf>) -> Result<Self> {
        if let Some(path) = config_path {
            ensure!(
                path.is_file(),
                "config file does not exist: {}",
                path.to_string_lossy()
            );
            Self::load_layered(path)
        } else {
            let mut c = Self::default();
            c.server
                .normalize_home_dir_inplace()
                .context("Failed to resolve server.home_dir (defaults)")?;
            Ok(c)
        }
    }

    /// Serialize configuration to YAML.
    ///
    /// # Errors
    /// Returns an error if serialization fails.
    pub fn to_yaml(&self) -> Result<String> {
        serde_saphyr::to_string(self).context("Failed to serialize config to YAML")
    }

    /// Deserialize a vendor configuration section into a typed struct.
    ///
    /// # Errors
    /// Returns `VendorConfigError::NotFound` if the vendor is not present,
    /// or `VendorConfigError::InvalidConfig` if deserialization fails.
    pub fn vendor_config<T: DeserializeOwned>(
        &self,
        vendor_name: &str,
    ) -> Result<T, VendorConfigError> {
        let raw = self
            .vendor
            .get(vendor_name)
            .ok_or_else(|| VendorConfigError::NotFound {
                vendor: vendor_name.to_owned(),
            })?;
        T::deserialize(raw).map_err(|e| VendorConfigError::InvalidConfig {
            vendor: vendor_name.to_owned(),
            source: e,
        })
    }

    /// Deserialize a vendor configuration section, returning `T::default()` if absent.
    ///
    /// # Errors
    /// Returns `VendorConfigError::InvalidConfig` if the section exists but cannot be
    /// deserialized into `T`.
    pub fn vendor_config_or_default<T: DeserializeOwned + Default>(
        &self,
        vendor_name: &str,
    ) -> Result<T, VendorConfigError> {
        let Some(raw) = self.vendor.get(vendor_name) else {
            return Ok(T::default());
        };
        T::deserialize(raw).map_err(|e| VendorConfigError::InvalidConfig {
            vendor: vendor_name.to_owned(),
            source: e,
        })
    }

    /// Apply overrides from command line arguments.
    pub fn apply_cli_overrides(&mut self, verbose: u8) {
        // Set logging level based on verbose flags for "default" section.
        if let Some(default_section) = self.logging.get_mut("default") {
            default_section.console_level = match verbose {
                0 => default_section.console_level, // keep
                1 => Some(Level::DEBUG),
                _ => Some(Level::TRACE),
            };
        }
    }
}

/// Command line arguments structure.
#[derive(Debug, Clone)]
pub struct CliArgs {
    pub config: Option<String>,
    pub print_config: bool,
    pub verbose: u8,
    pub mock: bool,
}

/// Parse YAML with duplicate-key rejection.
fn strict_yaml_parse<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, serde_saphyr::Error> {
    let opts = serde_saphyr::Options {
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        ..serde_saphyr::Options::default()
    };
    serde_saphyr::from_str_with_options(s, opts)
}

/// YAML [`Format`](figment::providers::Format) provider that rejects duplicate
/// mapping keys instead of silently keeping the last value.
///
/// Drop-in replacement for figment's built-in `Yaml` — use
/// `StrictYaml::file(path)` wherever you would use `Yaml::file(path)`.
struct StrictYaml;

impl figment::providers::Format for StrictYaml {
    type Error = serde_saphyr::Error;

    const NAME: &'static str = "YAML";

    fn from_str<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, Self::Error> {
        strict_yaml_parse(s)
    }
}

fn merge_module_files(
    bag: &mut HashMap<String, serde_json::Value>,
    dir: impl AsRef<Path>,
) -> Result<()> {
    use std::fs;
    let dir = dir.as_ref();
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext != "yml" && ext != "yaml" {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();
        let raw = fs::read_to_string(&path)?;
        let json: serde_json::Value = strict_yaml_parse(&raw)
            .with_context(|| format!("failed to parse module file: {}", path.display()))?;
        bag.insert(name, json);
    }
    Ok(())
}

// ---- New ModKit DB Handling Functions ----

/// Expands environment variables in a DSN string.
/// Replaces `${VARNAME}` with the actual environment variable value.
///
/// # Errors
/// Returns an error if any referenced env var is missing.
pub fn expand_env_in_dsn(dsn: &str) -> Result<String> {
    modkit_utils::var_expand::expand_env_vars(dsn).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Resolves password: if it contains ${VAR}, expands from environment variable; otherwise returns as-is.
///
/// # Errors
/// Returns an error if the referenced environment variable is not found.
pub fn resolve_password(password: Option<&str>) -> Result<Option<String>> {
    if let Some(pwd) = password {
        if pwd.starts_with("${") && pwd.ends_with('}') {
            // Extract variable name from ${VAR_NAME}
            let var_name = &pwd[2..pwd.len() - 1];
            let resolved = std::env::var(var_name).with_context(|| {
                format!("Environment variable '{var_name}' not found for password")
            })?;
            Ok(Some(resolved))
        } else {
            // Return literal password as-is
            Ok(Some(pwd.to_owned()))
        }
    } else {
        Ok(None)
    }
}

/// Validates that a DSN string is parseable by the dsn crate.
/// Note: `SQLite` DSNs have special formats that dsn crate doesn't recognize, so we skip validation for them.
///
/// # Errors
/// Returns an error if the DSN is invalid.
pub fn validate_dsn(dsn: &str) -> Result<()> {
    // Skip validation for SQLite DSNs as they use special syntax not recognized by dsn crate
    if dsn.starts_with("sqlite:") {
        return Ok(());
    }

    let _parsed = dsn::parse(dsn).map_err(|e| anyhow::anyhow!("Invalid DSN '{dsn}': {e}"))?;

    Ok(())
}

/// Resolves `SQLite` @`file()` syntax in DSN to actual file paths.
/// - `sqlite://@file(users.sqlite)` → `$HOME/.hyperspot/<module>/users.sqlite`
/// - `sqlite://@file(/abs/path/file.db)` → use absolute path
/// - `sqlite://` or `sqlite:///` → `$HOME/.hyperspot/<module>/<module>.sqlite`
fn resolve_sqlite_dsn(
    dsn: &str,
    home_dir: &Path,
    module_name: &str,
    dry_run: bool,
) -> Result<String> {
    if dsn.contains("@file(") {
        // Extract the file path from @file(...)
        if let Some(start) = dsn.find("@file(")
            && let Some(end) = dsn[start..].find(')')
        {
            let file_path = &dsn[start + 6..start + end]; // +6 for "@file("

            let resolved_path = if file_path.starts_with('/')
                || (file_path.len() > 1 && file_path.chars().nth(1) == Some(':'))
            {
                // Absolute path (Unix or Windows)
                PathBuf::from(file_path)
            } else {
                // Relative path - resolve under module directory
                let module_dir = home_dir.join(module_name);
                if !dry_run {
                    std::fs::create_dir_all(&module_dir).with_context(|| {
                        format!(
                            "Failed to create module directory: {}",
                            module_dir.display()
                        )
                    })?;
                }
                module_dir.join(file_path)
            };

            let normalized_path = normalize_path(&resolved_path);
            // For Windows absolute paths (C:/...), use sqlite:path format
            // For Unix absolute paths (/...), use sqlite://path format
            if normalized_path.len() > 1 && normalized_path.chars().nth(1) == Some(':') {
                // Windows absolute path like C:/...
                return Ok(format!("sqlite:{normalized_path}"));
            }
            // Unix absolute path or relative path
            return Ok(format!("sqlite://{normalized_path}"));
        }
        return Err(anyhow::anyhow!(
            "Invalid @file() syntax in SQLite DSN: {dsn}"
        ));
    }

    // Handle empty DSN or just sqlite:// - default to module.sqlite
    if dsn == "sqlite://" || dsn == "sqlite:///" || dsn == "sqlite:" {
        let module_dir = home_dir.join(module_name);
        if !dry_run {
            std::fs::create_dir_all(&module_dir).with_context(|| {
                format!(
                    "Failed to create module directory: {}",
                    module_dir.display()
                )
            })?;
        }
        let db_path = module_dir.join(format!("{module_name}.sqlite"));
        let normalized_path = normalize_path(&db_path);
        // For Windows absolute paths (C:/...), use sqlite:path format
        // For Unix absolute paths (/...), use sqlite://path format
        if normalized_path.len() > 1 && normalized_path.chars().nth(1) == Some(':') {
            // Windows absolute path like C:/...
            return Ok(format!("sqlite:{normalized_path}"));
        }
        // Unix absolute path or relative path
        return Ok(format!("sqlite://{normalized_path}"));
    }

    // Return DSN as-is for normal cases
    Ok(dsn.to_owned())
}

/// Builds a server-based DSN from individual fields.
/// Used when no base DSN is provided or when overriding DSN components.
/// Uses `url::Url` to properly handle percent-encoding of special characters.
fn build_server_dsn(
    scheme: &str,
    host: Option<&str>,
    port: Option<u16>,
    user: Option<&str>,
    password: Option<&str>,
    dbname: Option<&str>,
    params: &HashMap<String, String>,
) -> Result<String> {
    let host = host.unwrap_or("localhost");
    let user = user.unwrap_or("postgres"); // reasonable default for server-based DBs

    // Start with base URL
    let mut url = Url::parse(&format!("{scheme}://dummy/"))
        .with_context(|| format!("Invalid scheme: {scheme}"))?;

    // Set host (required)
    url.set_host(Some(host))
        .with_context(|| format!("Invalid host: {host}"))?;

    // Set port if provided
    if let Some(port) = port {
        url.set_port(Some(port))
            .map_err(|()| anyhow::anyhow!("Invalid port: {port}"))?;
    }

    // Set username
    url.set_username(user)
        .map_err(|()| anyhow::anyhow!("Failed to set username: {user}"))?;

    // Set password if provided
    if let Some(password) = password {
        url.set_password(Some(password))
            .map_err(|()| anyhow::anyhow!("Failed to set password"))?;
    }

    // Set database name as path (with leading slash)
    if let Some(dbname) = dbname {
        // Manually encode the dbname to handle special characters
        let encoded_dbname = urlencoding::encode(dbname);
        url.set_path(&format!("/{encoded_dbname}"));
    } else {
        url.set_path("/");
    }

    // Set query parameters
    if !params.is_empty() {
        // Use url::Url::query_pairs_mut() to properly handle encoding
        let mut query_pairs = url.query_pairs_mut();
        for (key, value) in params {
            query_pairs.append_pair(key, value);
        }
    }

    Ok(url.to_string())
}

/// Builds a `SQLite` DSN by replacing the database file path while preserving query parameters.
fn build_sqlite_dsn_with_dbname_override(
    original_dsn: &str,
    dbname: &str,
    module_name: &str,
    home_dir: &Path,
    dry_run: bool,
) -> Result<String> {
    // Parse the original DSN to extract query parameters
    let query_params = if let Some(query_start) = original_dsn.find('?') {
        &original_dsn[query_start..]
    } else {
        ""
    };

    // Build the correct path for the database file
    let module_dir = home_dir.join(module_name);
    if !dry_run {
        std::fs::create_dir_all(&module_dir).with_context(|| {
            format!(
                "Failed to create module directory: {}",
                module_dir.display()
            )
        })?;
    }
    let db_path = module_dir.join(dbname);
    let normalized_path = normalize_path(&db_path);

    // Build the new DSN with correct format for the platform
    let dsn_base = if normalized_path.len() > 1 && normalized_path.chars().nth(1) == Some(':') {
        // Windows absolute path like C:/...
        format!("sqlite:{normalized_path}")
    } else {
        // Unix absolute path or relative path
        format!("sqlite://{normalized_path}")
    };

    Ok(format!("{dsn_base}{query_params}"))
}

/// Builds a `SQLite` DSN from file/path or validates existing DSN.
/// If dbname is provided, it overrides the database file in the DSN.
///
/// # Arguments
/// * `dry_run` - If true, skip directory creation (for read-only inspection)
fn build_sqlite_dsn(
    dsn: Option<&str>,
    file: Option<&str>,
    path: Option<&PathBuf>,
    dbname: Option<&str>,
    module_name: &str,
    home_dir: &Path,
    dry_run: bool,
) -> Result<String> {
    // If full DSN provided, resolve @file() syntax and validate
    if let Some(dsn) = dsn {
        let resolved_dsn = resolve_sqlite_dsn(dsn, home_dir, module_name, dry_run)?;

        // If dbname is provided, we need to replace the database file path while preserving query params
        if let Some(dbname) = dbname {
            return build_sqlite_dsn_with_dbname_override(
                &resolved_dsn,
                dbname,
                module_name,
                home_dir,
                dry_run,
            );
        }

        validate_dsn(&resolved_dsn)?;
        return Ok(resolved_dsn);
    }

    // Build from path (absolute)
    if let Some(path) = path {
        let absolute_path = if path.is_absolute() {
            path.clone()
        } else {
            home_dir.join(path)
        };
        let normalized_path = normalize_path(&absolute_path);
        // For Windows absolute paths (C:/...), use sqlite:path format
        // For Unix absolute paths (/...), use sqlite://path format
        if normalized_path.len() > 1 && normalized_path.chars().nth(1) == Some(':') {
            // Windows absolute path like C:/...
            return Ok(format!("sqlite:{normalized_path}"));
        }
        // Unix absolute path or relative path
        return Ok(format!("sqlite://{normalized_path}"));
    }

    // Build from file (relative under module dir)
    if let Some(file) = file {
        let module_dir = home_dir.join(module_name);
        if !dry_run {
            std::fs::create_dir_all(&module_dir).with_context(|| {
                format!(
                    "Failed to create module directory: {}",
                    module_dir.display()
                )
            })?;
        }
        let db_path = module_dir.join(file);
        let normalized_path = normalize_path(&db_path);
        // For Windows absolute paths (C:/...), use sqlite:path format
        // For Unix absolute paths (/...), use sqlite://path format
        if normalized_path.len() > 1 && normalized_path.chars().nth(1) == Some(':') {
            // Windows absolute path like C:/...
            return Ok(format!("sqlite:{normalized_path}"));
        }
        // Unix absolute path or relative path
        return Ok(format!("sqlite://{normalized_path}"));
    }

    // Default to module.sqlite
    let module_dir = home_dir.join(module_name);
    if !dry_run {
        std::fs::create_dir_all(&module_dir).with_context(|| {
            format!(
                "Failed to create module directory: {}",
                module_dir.display()
            )
        })?;
    }
    let db_path = module_dir.join(format!("{module_name}.sqlite"));
    let normalized_path = normalize_path(&db_path);
    // For Windows absolute paths (C:/...), use sqlite:path format
    // For Unix absolute paths (/...), use sqlite://path format
    if normalized_path.len() > 1 && normalized_path.chars().nth(1) == Some(':') {
        // Windows absolute path like C:/...
        Ok(format!("sqlite:{normalized_path}"))
    } else {
        // Unix absolute path or relative path
        Ok(format!("sqlite://{normalized_path}"))
    }
}

/// Type alias for the complex return type of `build_final_db_for_module`
type DbConfigResult = Result<Option<(String /* final_dsn */, PoolCfg)>>;

/// Builder for accumulating database configuration from multiple sources
#[derive(Default)]
struct DbConfigBuilder {
    dsn: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    password: Option<String>,
    dbname: Option<String>,
    params: HashMap<String, String>,
    pool: PoolCfg,
}

impl DbConfigBuilder {
    fn new() -> Self {
        Self::default()
    }

    /// Apply global server configuration
    fn apply_global_server(
        &mut self,
        global_server: &DbConnConfig,
        home_dir: &Path,
        module_name: &str,
        dry_run: bool,
    ) -> Result<()> {
        // Apply global server DSN
        if let Some(global_dsn) = &global_server.dsn {
            let expanded_dsn = expand_env_in_dsn(global_dsn)?;
            // For SQLite, resolve @file() syntax before validation
            let resolved_dsn = if expanded_dsn.starts_with("sqlite") {
                resolve_sqlite_dsn(&expanded_dsn, home_dir, module_name, dry_run)?
            } else {
                expanded_dsn
            };
            validate_dsn(&resolved_dsn)?;
            self.dsn = Some(resolved_dsn);
        }

        // Apply global server fields (override DSN parts)
        if let Some(host) = &global_server.host {
            self.host = Some(host.clone());
        }
        if let Some(port) = global_server.port {
            self.port = Some(port);
        }
        if let Some(user) = &global_server.user {
            self.user = Some(user.clone());
        }
        if let Some(password) = resolve_password(global_server.password.as_deref())? {
            self.password = Some(password);
        }
        if let Some(dbname) = &global_server.dbname {
            self.dbname = Some(dbname.clone());
        }
        if let Some(params) = &global_server.params {
            self.params.extend(params.clone());
        }
        if let Some(pool) = &global_server.pool {
            self.pool = pool.clone();
        }

        Ok(())
    }

    /// Apply module DSN (overrides global DSN)
    fn apply_module_dsn(
        &mut self,
        module_dsn: &str,
        home_dir: &Path,
        module_name: &str,
        dry_run: bool,
    ) -> Result<()> {
        // For SQLite, resolve @file() syntax before validation
        let resolved_dsn = if module_dsn.starts_with("sqlite") {
            resolve_sqlite_dsn(module_dsn, home_dir, module_name, dry_run)?
        } else {
            module_dsn.to_owned()
        };
        validate_dsn(&resolved_dsn)?;
        self.dsn = Some(resolved_dsn);
        Ok(())
    }

    /// Apply module fields (override everything)
    fn apply_module_fields(&mut self, module_db_config: &DbConnConfig) -> Result<()> {
        if let Some(host) = &module_db_config.host {
            self.host = Some(host.clone());
        }
        if let Some(port) = module_db_config.port {
            self.port = Some(port);
        }
        if let Some(user) = &module_db_config.user {
            self.user = Some(user.clone());
        }
        if let Some(password) = resolve_password(module_db_config.password.as_deref())? {
            self.password = Some(password);
        }
        if let Some(dbname) = &module_db_config.dbname {
            self.dbname = Some(dbname.clone());
        }
        if let Some(params) = &module_db_config.params {
            self.params.extend(params.clone());
        }
        if let Some(pool) = &module_db_config.pool {
            // Module pool settings override global ones
            if let Some(max_conns) = pool.max_conns {
                self.pool.max_conns = Some(max_conns);
            }
            if let Some(acquire_timeout) = pool.acquire_timeout {
                self.pool.acquire_timeout = Some(acquire_timeout);
            }
        }
        Ok(())
    }

    /// Check if we have any field overrides that require rebuilding the DSN
    fn has_field_overrides(&self) -> bool {
        self.host.is_some()
            || self.port.is_some()
            || self.user.is_some()
            || self.password.is_some()
            || !self.params.is_empty()
    }
}

/// Determines the database backend type (`SQLite` or server-based)
fn decide_backend(builder: &DbConfigBuilder, module_db_config: &DbConnConfig) -> bool {
    // Always treat as SQLite if DSN starts with "sqlite", regardless of server reference
    // Also treat as SQLite if no server reference and no explicit DSN (default case)
    module_db_config.file.is_some()
        || module_db_config.path.is_some()
        || builder
            .dsn
            .as_ref()
            .is_some_and(|dsn| dsn.starts_with("sqlite"))
        || (module_db_config.server.is_none() && builder.dsn.is_none())
}

/// Finalize `SQLite` DSN from builder state
fn finalize_sqlite_dsn(
    builder: &DbConfigBuilder,
    module_db_config: &DbConnConfig,
    module_name: &str,
    home_dir: &Path,
    dry_run: bool,
) -> Result<String> {
    build_sqlite_dsn(
        builder.dsn.as_deref(),
        module_db_config.file.as_deref(),
        module_db_config.path.as_ref(),
        builder.dbname.as_deref(),
        module_name,
        home_dir,
        dry_run,
    )
}

/// Finalize server-based DSN from builder state
fn finalize_server_dsn(builder: &DbConfigBuilder, module_name: &str) -> Result<String> {
    // Extract dbname from DSN if not provided separately
    let dbname = if let Some(dbname) = builder.dbname.as_deref() {
        dbname.to_owned()
    } else if let Some(dsn) = builder.dsn.as_ref() {
        // Try to extract dbname from DSN path
        if let Ok(parsed) = url::Url::parse(dsn) {
            let path = parsed.path();
            if path.len() > 1 {
                // Remove leading slash and return the path as dbname
                path[1..].to_string()
            } else {
                return Err(anyhow::anyhow!(
                    "Server-based database config for module '{module_name}' missing required 'dbname'"
                ));
            }
        } else {
            return Err(anyhow::anyhow!(
                "Server-based database config for module '{module_name}' missing required 'dbname'"
            ));
        }
    } else {
        return Err(anyhow::anyhow!(
            "Server-based database config for module '{module_name}' missing required 'dbname'"
        ));
    };

    if builder.has_field_overrides() || builder.dsn.is_none() {
        // Build DSN from fields when we have overrides or no original DSN
        let scheme = if let Some(dsn) = &builder.dsn {
            let parsed = Url::parse(dsn)?;
            parsed.scheme().to_owned()
        } else {
            "postgresql".to_owned() // default
        };

        build_server_dsn(
            &scheme,
            builder.host.as_deref(),
            builder.port,
            builder.user.as_deref(),
            builder.password.as_deref(),
            Some(&dbname),
            &builder.params,
        )
    } else if let Some(original_dsn) = &builder.dsn {
        // Use original DSN when no field overrides (but update dbname if needed)
        if let Ok(mut parsed) = Url::parse(original_dsn) {
            // Update the path with the final dbname if it's different
            let original_dbname = parsed.path().trim_start_matches('/');
            if original_dbname != dbname {
                parsed.set_path(&format!("/{dbname}"));
            }
            Ok(parsed.to_string())
        } else {
            // Fallback to building from fields if URL parsing fails
            build_server_dsn(
                "postgresql",
                builder.host.as_deref(),
                builder.port,
                builder.user.as_deref(),
                builder.password.as_deref(),
                Some(&dbname),
                &builder.params,
            )
        }
    } else {
        // This branch should not be reachable due to the condition above
        unreachable!("final_dsn should not be None when has_field_overrides is false")
    }
}

/// Redacts password from DSN for logging
fn redact_dsn_for_logging(dsn: &str) -> Result<String> {
    if dsn.contains('@') {
        let parsed = Url::parse(dsn)?;
        let mut log_url = parsed;
        if log_url.password().is_some() {
            log_url.set_password(Some("***")).ok();
        }
        Ok(log_url.to_string())
    } else {
        Ok(dsn.to_owned())
    }
}

// ---- OoP Module Configuration Support ----

/// Environment variable name for passing rendered module config to `OoP` modules.
pub const MODKIT_MODULE_CONFIG_ENV: &str = "MODKIT_MODULE_CONFIG";

/// Rendered database configuration for `OoP` modules.
/// Contains both global server templates and module-specific config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedDbConfig {
    /// Global database configuration with server templates.
    /// `OoP` module can use these servers for reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<GlobalDatabaseConfig>,
    /// Module-specific database configuration (already merged with server reference in master).
    /// This is the `modules.<name>.database` section after server merge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<DbConnConfig>,
}

impl RenderedDbConfig {
    /// Create a new `RenderedDbConfig` from global and module database configurations.
    #[must_use]
    pub fn new(global: Option<GlobalDatabaseConfig>, module: Option<DbConnConfig>) -> Self {
        Self { global, module }
    }
}

/// Rendered module configuration passed to `OoP` modules via environment variable.
///
/// This struct contains everything an `OoP` module needs to initialize:
/// - Database configuration (structured, for field-by-field merge in `OoP`)
/// - Module config section
/// - Logging configuration (for key-by-key merge in `OoP`)
/// - OpenTelemetry configuration (resource, tracing, metrics)
///
/// The runtime section is excluded as it's only relevant for the master host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedModuleConfig {
    /// Rendered database configuration (structured, not resolved DSN).
    /// `OoP` module will merge this with local --config using field-by-field merge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<RenderedDbConfig>,
    /// Module-specific config section (passed as-is)
    #[serde(default)]
    pub config: serde_json::Value,
    /// Logging configuration from master host.
    /// `OoP` module will merge this with local --config (local keys override master keys).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<LoggingConfig>,
    /// OpenTelemetry configuration from master host (resource, tracing, metrics).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opentelemetry: Option<OpenTelemetryConfig>,
}

impl RenderedModuleConfig {
    /// Deserialize from JSON string (used when reading from env var).
    ///
    /// # Errors
    /// Returns an error if JSON parsing fails.
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("Failed to parse RenderedModuleConfig from JSON")
    }

    /// Serialize to JSON string (used when passing to `OoP` modules via env var).
    ///
    /// # Errors
    /// Returns an error if serialization fails.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).context("Failed to serialize RenderedModuleConfig to JSON")
    }
}

/// Render module configuration for passing to `OoP` module via environment variable.
///
/// This function prepares a structured configuration that an `OoP` module can use
/// to initialize itself. The configuration includes:
/// - Database configuration (structured, for field-by-field merge in `OoP`)
/// - Module config section
/// - Logging configuration (for key-by-key merge in `OoP`)
/// - Tracing configuration for OTEL
///
/// The runtime section is excluded as it's only relevant for the master host.
///
/// `OoP` modules receive this via `MODKIT_MODULE_CONFIG` env var and can override
/// any section with their local --config file.
///
/// # Errors
/// Returns an error if module configuration parsing fails.
pub fn render_module_config_for_oop(
    app: &AppConfig,
    module_name: &str,
    _home_dir: &std::path::Path,
) -> Result<RenderedModuleConfig> {
    // Get module's database config (with server reference, but NOT resolved to DSN).
    // OoP module will use DbManager to resolve this with its local overrides.
    let module_db_config = parse_module_config(app, module_name)
        .ok()
        .and_then(|entry| entry.database);

    // Build database config with global servers and module config (structured, not resolved)
    let database = if module_db_config.is_some() || app.database.is_some() {
        Some(RenderedDbConfig::new(
            app.database.clone(),
            module_db_config,
        ))
    } else {
        None
    };

    // Get the module's config section (excluding database and runtime)
    let config = parse_module_config(app, module_name)
        .map(|entry| entry.config)
        .unwrap_or_default();

    // Pass logging config from master host so OoP modules can merge with their local config
    let logging = app.logging.clone();

    // Pass OpenTelemetry config from master host so OoP modules use the same settings
    let opentelemetry = if app.opentelemetry.tracing.enabled || app.opentelemetry.metrics.enabled {
        Some(app.opentelemetry.clone())
    } else {
        None
    };

    Ok(RenderedModuleConfig {
        database,
        config,
        logging: Some(logging),
        opentelemetry,
    })
}

/// Parse a module config from the config bag.
///
/// # Errors
/// Returns an error if the module is not found or config parsing fails.
pub fn parse_module_config(app: &AppConfig, module_name: &str) -> Result<ModuleConfig> {
    let module_raw = app
        .modules
        .get(module_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Module '{module_name}' not found in config"))?;

    let module_config: ModuleConfig = serde_json::from_value(module_raw)?;
    Ok(module_config)
}

/// Helper to get runtime config for a module (if present).
///
/// # Errors
/// Returns an error if module config parsing fails.
pub fn get_module_runtime_config(
    app: &AppConfig,
    module_name: &str,
) -> Result<Option<ModuleRuntime>> {
    let entry = parse_module_config(app, module_name)?;
    Ok(entry.runtime)
}

/// Merges global + module DB configs into a final, validated DSN and pool config.
/// Precedence: Global DSN -> Global fields -> Module DSN -> Module fields (fields always win).
/// For server-based, returns error if final dbname is missing.
/// For `SQLite`, builds/normalizes sqlite DSN from file/path or uses a full DSN as-is.
///
/// # Arguments
/// * `dry_run` - If true, skip directory creation (for read-only inspection)
///
/// # Errors
/// Returns an error if database configuration is invalid or resolution fails.
pub fn build_final_db_for_module(
    app: &AppConfig,
    module_name: &str,
    home_dir: &Path,
    dry_run: bool,
) -> DbConfigResult {
    // Parse module entry from raw JSON
    let Some(module_raw) = app.modules.get(module_name) else {
        return Ok(None); // No module config
    };

    let module_entry: ModuleConfig = serde_json::from_value(module_raw.clone())
        .with_context(|| format!("Invalid module config structure for '{module_name}'"))?;

    let Some(module_db_config) = module_entry.database else {
        tracing::warn!(
            "Module '{}' has no database configuration; DB capability disabled",
            module_name
        );
        return Ok(None);
    };

    // Global database config
    let global_db_config = app.database.as_ref();

    // Build configuration using the builder pattern
    let mut builder = DbConfigBuilder::new();

    // Step 1: Apply global server config if referenced
    if let Some(server_name) = &module_db_config.server {
        let global_server = global_db_config
            .and_then(|gc| gc.servers.get(server_name))
            .ok_or_else(|| {
                anyhow::anyhow!("Referenced server '{server_name}' not found in global config")
            })?;

        builder.apply_global_server(global_server, home_dir, module_name, dry_run)?;
    }

    // Step 2: Apply module DSN (override global)
    if let Some(module_dsn) = &module_db_config.dsn {
        builder.apply_module_dsn(module_dsn, home_dir, module_name, dry_run)?;
    }

    // Step 3: Apply module fields (override everything)
    builder.apply_module_fields(&module_db_config)?;

    // Determine backend type and finalize DSN
    let is_sqlite = decide_backend(&builder, &module_db_config);

    let result_dsn = if is_sqlite {
        finalize_sqlite_dsn(&builder, &module_db_config, module_name, home_dir, dry_run)?
    } else {
        finalize_server_dsn(&builder, module_name)?
    };

    // Validate final DSN
    validate_dsn(&result_dsn)?;

    // Redact password for logging
    let log_dsn = redact_dsn_for_logging(&result_dsn)?;

    tracing::info!(
        "Built final DB config for module '{}': {}",
        module_name,
        log_dsn
    );

    Ok(Some((result_dsn, builder.pool)))
}

/// Helper function to get module database configuration from `AppConfig`.
/// Returns the `DbConnConfig` for a module, or None if the module has no database config.
#[must_use]
pub fn get_module_db_config(app: &AppConfig, module_name: &str) -> Option<DbConnConfig> {
    let module_raw = app.modules.get(module_name)?;
    let module_entry: ModuleConfig = serde_json::from_value(module_raw.clone()).ok()?;
    module_entry.database
}

/// Helper function to resolve module home directory.
/// Returns the path where module-specific files (like `SQLite` databases) should be stored.
#[must_use]
pub fn module_home(app: &AppConfig, module_name: &str) -> PathBuf {
    PathBuf::from(&app.server.home_dir).join(module_name)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "mod_tests.rs"]
mod tests;

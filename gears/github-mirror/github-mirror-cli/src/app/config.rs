use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use figment::Figment;
use figment::providers::Serialized;
use github_mirror::domain::validate::validate_repo_path;
use serde::Deserialize;
use serde_json::{Value, json};
use toolkit::bootstrap::AppConfig;
use uuid::Uuid;

pub const DEFAULT_TEMPLATE: &str = include_str!("../default_config.toml");

pub struct CliConfig {
    pub app: AppConfig,
    pub tenant_id: Option<Uuid>,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabasePlacement {
    #[value(name = "per_repo")]
    PerRepo,
    #[value(name = "per_org")]
    PerOrg,
    #[value(name = "shared")]
    Shared,
}

pub struct StorageFlags<'a> {
    pub storage_dir: Option<PathBuf>,
    pub database_url: Option<String>,
    pub database_placement: Option<DatabasePlacement>,
    pub repo: Option<&'a str>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CliTable {
    tenant_id: Option<Uuid>,
    database_placement: Option<DatabasePlacement>,
}

pub fn load(
    path: &Path,
    github_token: Option<&str>,
    storage: StorageFlags<'_>,
) -> Result<CliConfig> {
    let mut merged: Value =
        toml::from_str(DEFAULT_TEMPLATE).context("the built-in configuration does not parse")?;
    if path.is_file() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let overrides: Value = toml::from_str(&text)
            .with_context(|| format!("{} is not valid TOML", path.display()))?;
        merge(&mut merged, overrides);
    }

    let cli = take_cli_table(&mut merged)?;
    if let Some(dir) = storage.storage_dir {
        merged["server"]["home_dir"] = Value::String(dir.to_string_lossy().into_owned());
    }
    place_database(
        &mut merged,
        storage.database_url,
        storage
            .database_placement
            .or(cli.database_placement)
            .unwrap_or(DatabasePlacement::PerRepo),
        storage.repo,
    )?;
    match github_token {
        Some(token) => add_github_token(&mut merged, token)?,
        None => drop_github_token(&mut merged),
    }
    expand_home_dir(&mut merged)?;

    let app: AppConfig = Figment::new()
        .merge(Serialized::defaults(AppConfig::default()))
        .merge(Serialized::defaults(merged))
        .extract()
        .context("the configuration does not match the runtime's settings")?;
    Ok(CliConfig {
        app,
        tenant_id: cli.tenant_id,
    })
}

pub fn sqlite_file(app: &AppConfig) -> Option<PathBuf> {
    let database = app.gears.get("github-mirror")?.get("database")?;
    if let Some(dsn) = database.get("dsn").and_then(Value::as_str) {
        let path = dsn
            .strip_prefix("sqlite://")
            .or_else(|| dsn.strip_prefix("sqlite:"))?
            .split('?')
            .next()?;
        return (!path.is_empty() && path != ":memory:").then(|| PathBuf::from(path));
    }
    let file = database
        .get("path")
        .or_else(|| database.get("file"))?
        .as_str()?;
    let path = PathBuf::from(file);
    Some(if path.is_absolute() {
        path
    } else {
        app.server.home_dir.join("github-mirror").join(path)
    })
}

pub fn resolve_token(given: Option<String>, file_name: &str) -> Option<String> {
    if let Some(token) = given
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
    {
        return Some(token);
    }
    let path = user_home().ok()?.join(".github-mirror").join(file_name);
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|token| !token.is_empty())
}

fn merge(base: &mut Value, overrides: Value) {
    match (base, overrides) {
        (Value::Object(base), Value::Object(overrides)) => {
            for (key, value) in overrides {
                match base.get_mut(&key) {
                    Some(slot) => merge(slot, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overrides) => *base = overrides,
    }
}

fn take_cli_table(config: &mut Value) -> Result<CliTable> {
    match config.as_object_mut().and_then(|root| root.remove("cli")) {
        Some(cli) => serde_json::from_value(cli).context("the [cli] table is not valid"),
        None => Ok(CliTable::default()),
    }
}

fn place_database(
    config: &mut Value,
    url: Option<String>,
    placement: DatabasePlacement,
    repo: Option<&str>,
) -> Result<()> {
    let database = config
        .pointer_mut("/gears/github-mirror/database")
        .ok_or_else(|| anyhow!("gears.github-mirror.database is missing"))?;
    if let Some(url) = url {
        *database = json!({ "dsn": url });
        return Ok(());
    }
    let (Some(file), Some(repo)) = (
        database
            .get("file")
            .and_then(Value::as_str)
            .map(str::to_owned),
        repo,
    ) else {
        return Ok(());
    };
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| anyhow!("`{repo}` is not ORG/REPO"))?;
    validate_repo_path(owner, name).map_err(|e| anyhow!("`{repo}`: {e}"))?;
    let folder = match placement {
        DatabasePlacement::Shared => return Ok(()),
        DatabasePlacement::PerOrg => owner.to_owned(),
        DatabasePlacement::PerRepo => format!("{owner}_{name}"),
    };
    database["file"] = Value::String(format!("cache/{folder}/{file}"));
    Ok(())
}

fn add_github_token(config: &mut Value, token: &str) -> Result<()> {
    let secret = config
        .pointer("/gears/github-mirror/config/github_token_secret")
        .cloned()
        .ok_or_else(|| anyhow!("gears.github-mirror.config.github_token_secret is missing"))?;
    let entry = json!({
        "tenant_id": secret.get("tenant_id"),
        "key": secret.get("key"),
        "value": token,
    });
    config
        .pointer_mut("/gears/static-credstore-plugin/config/secrets")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("gears.static-credstore-plugin.config.secrets must be a list"))?
        .push(entry);
    Ok(())
}

fn drop_github_token(config: &mut Value) {
    if let Some(gear) = config
        .pointer_mut("/gears/github-mirror/config")
        .and_then(Value::as_object_mut)
    {
        gear.remove("github_token_secret");
    }
}

fn expand_home_dir(config: &mut Value) -> Result<()> {
    let Some(home_dir) = config.pointer_mut("/server/home_dir") else {
        return Ok(());
    };
    let Some(raw) = home_dir.as_str() else {
        bail!("server.home_dir must be a string");
    };
    let path = match raw.strip_prefix("~/") {
        Some(rest) => user_home()?.join(rest),
        None if raw == "~" => user_home()?,
        None => PathBuf::from(raw),
    };
    std::fs::create_dir_all(&path).with_context(|| format!("cannot create {}", path.display()))?;
    *home_dir = Value::String(path.to_string_lossy().into_owned());
    Ok(())
}

fn user_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            anyhow!("cannot find the home directory: neither HOME nor USERPROFILE is set")
        })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod tests {
    use super::*;

    fn scratch_dir() -> PathBuf {
        std::env::temp_dir().join(format!("github-mirror-cli-{}", Uuid::new_v4().simple()))
    }

    fn flags(
        dir: &Path,
        placement: Option<DatabasePlacement>,
        repo: Option<&'static str>,
    ) -> StorageFlags<'static> {
        StorageFlags {
            storage_dir: Some(dir.to_path_buf()),
            database_url: None,
            database_placement: placement,
            repo,
        }
    }

    fn database_file(
        dir: &Path,
        placement: Option<DatabasePlacement>,
        repo: Option<&'static str>,
    ) -> Option<PathBuf> {
        let loaded = load(
            Path::new("does-not-exist.toml"),
            None,
            flags(dir, placement, repo),
        )
        .unwrap();
        sqlite_file(&loaded.app)
    }

    fn remove(dir: &Path) {
        if dir.exists() {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    fn under_cache(dir: &Path, folder: &str) -> PathBuf {
        dir.join("github-mirror")
            .join("cache")
            .join(folder)
            .join("github_mirror.db")
    }

    #[test]
    fn per_repo_is_the_default_and_names_the_file_after_owner_and_repository() {
        let dir = scratch_dir();
        assert_eq!(
            database_file(&dir, None, Some("dtolnay/itoa")),
            Some(under_cache(&dir, "dtolnay_itoa"))
        );
        remove(&dir);
    }

    #[test]
    fn per_org_and_shared_place_the_file_by_owner_or_once_for_all() {
        let dir = scratch_dir();
        assert_eq!(
            database_file(&dir, Some(DatabasePlacement::PerOrg), Some("dtolnay/itoa")),
            Some(under_cache(&dir, "dtolnay"))
        );
        let shared = dir.join("github-mirror").join("github_mirror.db");
        assert_eq!(
            database_file(&dir, Some(DatabasePlacement::Shared), Some("dtolnay/itoa")),
            Some(shared.clone())
        );
        assert_eq!(database_file(&dir, None, None), Some(shared));
        remove(&dir);
    }

    #[test]
    fn a_database_url_replaces_the_file_and_makes_placement_moot() {
        let dir = scratch_dir();
        let postgres = load(
            Path::new("does-not-exist.toml"),
            None,
            StorageFlags {
                storage_dir: Some(dir.clone()),
                database_url: Some("postgres://u:p@db.local/mirror".to_owned()),
                database_placement: Some(DatabasePlacement::PerRepo),
                repo: Some("dtolnay/itoa"),
            },
        )
        .unwrap();
        assert_eq!(sqlite_file(&postgres.app), None);

        let by_url = dir.join("by-url.db");
        let sqlite = load(
            Path::new("does-not-exist.toml"),
            None,
            StorageFlags {
                storage_dir: Some(dir.clone()),
                database_url: Some(format!("sqlite://{}?mode=rwc", by_url.display())),
                database_placement: None,
                repo: Some("dtolnay/itoa"),
            },
        )
        .unwrap();
        assert_eq!(sqlite_file(&sqlite.app), Some(by_url));
        remove(&dir);
    }

    #[test]
    fn the_flag_beats_the_toml_placement() {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let toml = dir.join("github-mirror.toml");
        std::fs::write(&toml, "[cli]\ndatabase_placement = \"shared\"\n").unwrap();

        let from_toml = load(&toml, None, flags(&dir, None, Some("dtolnay/itoa"))).unwrap();
        assert_eq!(
            sqlite_file(&from_toml.app),
            Some(dir.join("github-mirror").join("github_mirror.db"))
        );
        let from_flag = load(
            &toml,
            None,
            flags(&dir, Some(DatabasePlacement::PerRepo), Some("dtolnay/itoa")),
        )
        .unwrap();
        assert_eq!(
            sqlite_file(&from_flag.app),
            Some(under_cache(&dir, "dtolnay_itoa"))
        );
        remove(&dir);
    }

    #[test]
    fn a_repository_name_that_is_not_a_safe_path_is_refused_before_any_file_is_chosen() {
        let dir = scratch_dir();
        assert!(
            load(
                Path::new("does-not-exist.toml"),
                None,
                flags(&dir, None, Some("../x"))
            )
            .is_err()
        );
        assert!(!dir.exists());
    }
}

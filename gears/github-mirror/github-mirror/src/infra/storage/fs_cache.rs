use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::github::cache::{CacheKey, CachedResponse, HttpCache};
use crate::infra::github::compression::{Compression, content_hash};

const META_SUFFIX: &str = ".meta.json";

#[derive(Debug, Serialize, Deserialize)]
struct Meta {
    url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    next_page: Option<String>,
    compression: String,
    content_hash: String,
    fetched_at: DateTime<Utc>,
}

pub struct FilesystemHttpCache {
    root: PathBuf,
    compression: Compression,
}

impl FilesystemHttpCache {
    #[must_use]
    pub fn new(root: PathBuf, compression: Compression) -> Self {
        Self { root, compression }
    }

    fn partition(&self, tenant_id: Uuid) -> PathBuf {
        self.root.join(tenant_id.to_string())
    }

    fn paths(&self, tenant_id: Uuid, key: &CacheKey) -> (PathBuf, PathBuf) {
        let key = key.as_str();
        let dir = self.partition(tenant_id).join(&key[..2]);
        (dir.join(key), dir.join(format!("{key}{META_SUFFIX}")))
    }

    async fn entries(&self, tenant_id: Uuid) -> Result<Vec<(PathBuf, PathBuf, Meta)>, DomainError> {
        let mut entries = Vec::new();
        for dir in list_dir(&self.partition(tenant_id)).await? {
            for (body_path, meta_path) in list_dir(&dir)
                .await?
                .into_iter()
                .filter_map(|path| body_of(&path).map(|body| (body, path)))
            {
                let Some(raw) = read_optional(&meta_path).await? else {
                    continue;
                };
                let Ok(meta) = serde_json::from_slice::<Meta>(&raw) else {
                    continue;
                };
                entries.push((body_path, meta_path, meta));
            }
        }
        Ok(entries)
    }

    async fn remove_where(
        &self,
        tenant_id: Uuid,
        doomed: impl Fn(&Meta) -> bool + Send + Sync,
    ) -> Result<u64, DomainError> {
        let mut removed = 0;
        for (body_path, meta_path, meta) in self.entries(tenant_id).await? {
            if doomed(&meta) {
                remove_optional(&body_path).await?;
                remove_optional(&meta_path).await?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn body_of(meta_path: &Path) -> Option<PathBuf> {
    let name = meta_path.file_name()?.to_str()?.strip_suffix(META_SUFFIX)?;
    Some(meta_path.with_file_name(name))
}

fn below(url: &str, prefix: &str) -> bool {
    url.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('?'))
}

fn below_any(url: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| below(url, prefix))
}

fn io_failure(action: &str, e: &std::io::Error) -> DomainError {
    DomainError::internal(format!("cache file {action} failed: {e}"))
}

async fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, DomainError> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_failure("read", &e)),
    }
}

async fn size_of(path: &Path) -> Result<u64, DomainError> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => Ok(meta.len()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(0),
        Err(e) => Err(io_failure("read", &e)),
    }
}

async fn remove_optional(path: &Path) -> Result<(), DomainError> {
    match tokio::fs::remove_file(path).await {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(io_failure("delete", &e)),
        _ => Ok(()),
    }
}

async fn list_dir(dir: &Path) -> Result<Vec<PathBuf>, DomainError> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_failure("listing", &e)),
    };
    let mut paths = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| io_failure("listing", &e))?
    {
        paths.push(entry.path());
    }
    Ok(paths)
}

async fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), DomainError> {
    let staging = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    tokio::fs::write(&staging, bytes)
        .await
        .map_err(|e| io_failure("write", &e))?;
    tokio::fs::rename(&staging, path)
        .await
        .map_err(|e| io_failure("write", &e))
}

fn decode(meta: &[u8], stored: &[u8]) -> Result<CachedResponse, DomainError> {
    let meta: Meta = serde_json::from_slice(meta)
        .map_err(|e| DomainError::internal(format!("cache metadata is unreadable: {e}")))?;
    let body = Compression::parse(&meta.compression)?.decompress(stored)?;
    if content_hash(&body) != meta.content_hash {
        return Err(DomainError::internal(format!(
            "cached body for {} failed its integrity check",
            meta.url
        )));
    }
    Ok(CachedResponse {
        body: String::from_utf8(body)
            .map_err(|e| DomainError::internal(format!("cached body is not UTF-8: {e}")))?,
        etag: meta.etag,
        last_modified: meta.last_modified,
        next_page: meta.next_page,
    })
}

#[async_trait]
impl HttpCache for FilesystemHttpCache {
    async fn get(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        key: &CacheKey,
    ) -> Result<Option<CachedResponse>, DomainError> {
        if scope.is_deny_all() {
            return Ok(None);
        }
        let (body_path, meta_path) = self.paths(tenant_id, key);
        let Some(meta) = read_optional(&meta_path).await? else {
            return Ok(None);
        };
        let Some(stored) = read_optional(&body_path).await? else {
            return Ok(None);
        };
        match decode(&meta, &stored) {
            Ok(entry) => Ok(Some(entry)),
            Err(e) => {
                tracing::warn!(error = %e, "discarding an unusable cache entry");
                Ok(None)
            }
        }
    }

    async fn put(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        key: &CacheKey,
        url: &str,
        entry: CachedResponse,
    ) -> Result<(), DomainError> {
        if scope.is_deny_all() {
            return Err(DomainError::forbidden(format!(
                "tenant {tenant_id} not in scope"
            )));
        }
        let plain = entry.body.as_bytes();
        let meta = Meta {
            url: url.to_owned(),
            etag: entry.etag,
            last_modified: entry.last_modified,
            next_page: entry.next_page,
            compression: self.compression.as_str().to_owned(),
            content_hash: content_hash(plain),
            fetched_at: Utc::now(),
        };
        let meta = serde_json::to_vec_pretty(&meta)
            .map_err(|e| DomainError::internal(format!("cache metadata did not encode: {e}")))?;

        let (body_path, meta_path) = self.paths(tenant_id, key);
        if let Some(dir) = body_path.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| io_failure("write", &e))?;
        }
        write_atomically(&body_path, &self.compression.compress(plain)?).await?;
        write_atomically(&meta_path, &meta).await
    }

    async fn clear(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        url_prefixes: &[&str],
    ) -> Result<u64, DomainError> {
        if scope.is_deny_all() || url_prefixes.is_empty() {
            return Ok(0);
        }
        self.remove_where(tenant_id, |meta| below_any(&meta.url, url_prefixes))
            .await
    }

    async fn expire(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        fetched_before: DateTime<Utc>,
    ) -> Result<u64, DomainError> {
        if scope.is_deny_all() {
            return Ok(0);
        }
        self.remove_where(tenant_id, |meta| meta.fetched_at < fetched_before)
            .await
    }

    async fn size(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        url_prefixes: &[&str],
    ) -> Result<u64, DomainError> {
        if scope.is_deny_all() || url_prefixes.is_empty() {
            return Ok(0);
        }
        let mut total = 0;
        for (body_path, meta_path, meta) in self.entries(tenant_id).await? {
            if below_any(&meta.url, url_prefixes) {
                total += size_of(&body_path).await? + size_of(&meta_path).await?;
            }
        }
        Ok(total)
    }
}

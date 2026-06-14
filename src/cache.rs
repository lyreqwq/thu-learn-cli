//! Small disk cache for slow-changing data such as the current semester and course list.
//! Time-sensitive homework and announcement data is never cached.

use anyhow::Result;
use serde::{de::DeserializeOwned, Serialize};
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use crate::paths;

fn cache_dir() -> PathBuf {
    paths::cache_dir()
}

fn cache_file(key: &str) -> PathBuf {
    // Cache keys are controlled semester strings and static names.
    cache_dir().join(format!("{key}.json"))
}

/// Returns a fresh cache hit, or runs `fut` and stores the result.
pub async fn with_cache<T, F>(key: &str, ttl: Duration, fut: F) -> Result<T>
where
    T: Serialize + DeserializeOwned,
    F: Future<Output = Result<T>>,
{
    let path = cache_file(key);
    if let Ok(meta) = std::fs::metadata(&path) {
        if let Ok(modified) = meta.modified() {
            let fresh = modified.elapsed().map(|e| e < ttl).unwrap_or(false);
            if fresh {
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(v) = serde_json::from_slice::<T>(&bytes) {
                        return Ok(v);
                    }
                }
            }
        }
    }

    let value = fut.await?;
    if let Ok(bytes) = serde_json::to_vec(&value) {
        std::fs::create_dir_all(cache_dir()).ok();
        std::fs::write(&path, bytes).ok();
    }
    Ok(value)
}

/// Clears cache after login to avoid stale data across accounts.
pub fn clear() {
    std::fs::remove_dir_all(cache_dir()).ok();
}

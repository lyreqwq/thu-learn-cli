//! Cookie and cache paths.

use directories::BaseDirs;
use std::path::PathBuf;

fn home_dir() -> PathBuf {
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .expect("Could not locate user home directory")
}

fn config_dir() -> PathBuf {
    home_dir().join(".config/thu-learn-cli")
}

fn cache_root() -> PathBuf {
    home_dir().join(".cache/thu-learn-cli")
}

pub fn cookie_path() -> PathBuf {
    config_dir().join("cookies.json")
}

/// Cache directory for slow-changing semester and course-list data.
pub fn cache_dir() -> PathBuf {
    cache_root()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_path_uses_config_dir() {
        let home = home_dir();
        let path = cookie_path();

        assert!(path.is_absolute());
        assert!(path.starts_with(&home));
        assert!(path.ends_with(".config/thu-learn-cli/cookies.json"));
    }

    #[test]
    fn cache_dir_uses_cache_dir() {
        let home = home_dir();
        let path = cache_dir();

        assert!(path.is_absolute());
        assert!(path.starts_with(&home));
        assert!(path.ends_with(".cache/thu-learn-cli"));
        assert!(!path.ends_with("cookies.json"));
    }
}

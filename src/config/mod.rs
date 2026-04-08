/// Configuration loading and hot-reload.

pub mod types;
pub mod watcher;

pub use types::{AppOverride, Config, DndConfig, DndMode, DndSchedule};

use std::path::{Path, PathBuf};

/// Default config file path.
pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("lunaris/notifications.toml")
}

/// Load config from a TOML file. Returns defaults if file is missing.
pub fn load_config(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(contents) => match toml::from_str::<Config>(&contents) {
            Ok(c) => {
                tracing::info!("loaded config from {}", path.display());
                c
            }
            Err(e) => {
                tracing::warn!("failed to parse {}: {e}, using defaults", path.display());
                Config::default()
            }
        },
        Err(_) => {
            tracing::info!("no config at {}, using defaults", path.display());
            Config::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_load_missing_file() {
        let c = load_config(Path::new("/nonexistent/path.toml"));
        assert_eq!(c.dnd.mode, DndMode::Off);
    }

    #[test]
    fn test_load_valid_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, "[dnd]\nmode = \"on\"\n").unwrap();

        let c = load_config(&path);
        assert_eq!(c.dnd.mode, DndMode::On);
    }

    #[test]
    fn test_load_invalid_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "{{{{invalid").unwrap();

        let c = load_config(&path);
        assert_eq!(c.dnd.mode, DndMode::Off); // defaults
    }
}

/// File watcher for config hot-reload.
///
/// Watches the parent directory of the config file (editors do atomic
/// rename, not in-place write). On change, re-loads the config and
/// sends the new value through a broadcast channel.

use std::path::{Path, PathBuf};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;

use super::{load_config, Config};

/// Start watching a config file for changes.
///
/// Returns a broadcast receiver that emits the new `Config` whenever
/// the file changes. The watcher runs in a background thread.
pub fn watch_config(
    path: PathBuf,
) -> Result<(broadcast::Receiver<Config>, RecommendedWatcher), notify::Error> {
    let (tx, rx) = broadcast::channel::<Config>(8);
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let watch_path = path.clone();

    let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
        let Ok(event) = res else { return };
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                // Only react to our specific file.
                let is_our_file = event.paths.iter().any(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy() == file_name)
                        .unwrap_or(false)
                });
                if is_our_file {
                    let new_config = load_config(&watch_path);
                    let _ = tx.send(new_config);
                    tracing::info!("config reloaded");
                }
            }
            _ => {}
        }
    })?;

    // Watch parent directory (editors rename, not modify).
    let parent = path.parent().unwrap_or(Path::new("."));
    if !parent.exists() {
        let _ = std::fs::create_dir_all(parent);
    }
    watcher.watch(parent, RecursiveMode::NonRecursive)?;

    tracing::info!("watching config at {}", path.display());

    Ok((rx, watcher))
}

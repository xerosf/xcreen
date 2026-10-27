use crate::modules::config::AppConfig;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{debug, warn};

/// File watcher for config.json changes
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

/// Starts a config file watcher that calls the callback when config.json changes
pub fn start_config_watcher<F>(on_change: F) -> Result<ConfigWatcher, String>
where
    F: Fn() + Send + 'static,
{
    let config_path =
        AppConfig::get_config_path().map_err(|e| format!("Failed to get config path: {}", e))?;

    let config_dir = config_path
        .parent()
        .ok_or("Failed to get config directory")?
        .to_path_buf();

    debug!("Setting up file watcher for: {}", config_path.display());

    let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        let event = match res {
            Ok(event) => event,
            Err(err) => {
                warn!("Config watch failed: {err}");
                return;
            }
        };
        if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
            let is_config = event
                .paths
                .iter()
                .any(|p| p.file_name() == Some(std::ffi::OsStr::new("config.json")));

            if is_config {
                on_change();
            }
        }
    })
    .map_err(|e| format!("Failed to create watcher: {}", e))?;

    let mut watcher = watcher;
    watcher
        .watch(&config_dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("Failed to watch directory: {}", e))?;

    debug!("Config watcher started for: {}", config_dir.display());

    Ok(ConfigWatcher { _watcher: watcher })
}

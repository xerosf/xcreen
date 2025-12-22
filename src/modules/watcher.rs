use std::sync::Arc;
use std::time::Duration;
use notify::{Watcher, RecursiveMode, Event, EventKind, RecommendedWatcher};
use log::{info, error, debug};
use crate::modules::config::AppConfig;

/// File watcher for config.json changes
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

/// Starts a config file watcher that calls the callback when config.json changes
pub fn start_config_watcher<F>(on_change: F) -> Result<ConfigWatcher, String>
where
    F: Fn(AppConfig) + Send + Sync + 'static,
{
    let config_path = AppConfig::get_config_path()
        .map_err(|e| format!("Failed to get config path: {}", e))?;
    
    let config_dir = config_path.parent()
        .ok_or("Failed to get config directory")?
        .to_path_buf();
    
    debug!("Setting up file watcher for: {}", config_path.display());
    
    let on_change = Arc::new(on_change);
    
    let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res
            && let EventKind::Modify(_) = event.kind {
                let is_config = event.paths.iter()
                    .any(|p| p.file_name() == Some(std::ffi::OsStr::new("config.json")));
                
                if is_config {
                    // Debounce rapid changes
                    std::thread::sleep(Duration::from_millis(100));
                    
                    match AppConfig::load() {
                        Ok(new_config) => {
                            on_change(new_config);
                            info!("Configuration reloaded");
                        }
                        Err(e) => error!("Failed to reload config: {}", e),
                    }
                }
            }
    }).map_err(|e| format!("Failed to create watcher: {}", e))?;
    
    let mut watcher = watcher;
    watcher.watch(&config_dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("Failed to watch directory: {}", e))?;
    
    info!("Config watcher started for: {}", config_dir.display());
    
    Ok(ConfigWatcher { _watcher: watcher })
}
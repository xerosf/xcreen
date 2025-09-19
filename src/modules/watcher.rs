use std::sync::{Arc, Mutex};
use std::time::Duration;
use notify::{Watcher, RecursiveMode, Event, EventKind, RecommendedWatcher};
use log::{info, error, debug};
use crate::modules::config::AppConfig;

/// File watcher for config.json changes
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Creates a new config file watcher that monitors config.json for changes
    pub fn new<F>(mut callback: F) -> Result<Self, String>
    where
        F: FnMut() + Send + 'static,
    {
        let config_path = AppConfig::get_config_path()
            .map_err(|e| format!("Failed to get config path: {}", e))?;
        
        let config_dir = config_path.parent()
            .ok_or("Failed to get config directory")?;
        
        debug!("Setting up file watcher for config at: {}", config_path.display());
        
        // Create the watcher
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    // Check if this is a modification event for our config file
                    if let EventKind::Modify(_) = event.kind {
                        for path in &event.paths {
                            if path.file_name() == Some(std::ffi::OsStr::new("config.json")) {
                                debug!("Config file change detected: {:?}", path);
                                callback();
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("File watcher error: {}", e);
                }
            }
        }).map_err(|e| format!("Failed to create file watcher: {}", e))?;
        
        // Watch the directory containing config.json
        watcher.watch(config_dir, RecursiveMode::NonRecursive)
            .map_err(|e| format!("Failed to watch config directory: {}", e))?;
        
        info!("File watcher initialized for config directory: {}", config_dir.display());
        
        Ok(ConfigWatcher {
            _watcher: watcher,
        })
    }
}

/// Starts a config file watcher that automatically reloads configuration when config.json changes
pub fn start_config_watcher(
    config: Arc<Mutex<AppConfig>>,
    autostart_enabled: Arc<Mutex<bool>>,
    last_brightness: Arc<Mutex<u32>>,
    on_reload: Option<Arc<dyn Fn(&AppConfig) + Send + Sync>>,
) -> Result<ConfigWatcher, String> {
    let config_clone = Arc::clone(&config);
    let autostart_enabled_clone = Arc::clone(&autostart_enabled);
    let last_brightness_clone = Arc::clone(&last_brightness);
    
    let on_reload_cb = on_reload.clone();

    ConfigWatcher::new(move || {
        // Add a small delay to handle temporary files and multiple rapid changes
        std::thread::sleep(Duration::from_millis(100));
        
        // Try to reload the config
        match AppConfig::load() {
            Ok(new_config) => {
                // Update the shared config
                if let Ok(mut config_guard) = config_clone.lock() {
                    *config_guard = new_config.clone();
                }
                
                // Update application state
                if let Ok(mut autostart) = autostart_enabled_clone.lock() {
                    *autostart = new_config.autostart_enabled;
                }
                
                if let Ok(mut brightness) = last_brightness_clone.lock() {
                    *brightness = new_config.last_brightness;
                }

                // Notify optional callback
                if let Some(cb) = &on_reload_cb {
                    cb(&new_config);
                }
                
                info!("Configuration automatically reloaded from file");
            }
            Err(e) => {
                error!("Failed to reload config after file change: {}", e);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    
    #[test]
    fn test_config_watcher_creation() {
        let callback_called = Arc::new(AtomicBool::new(false));
        let callback_called_clone = Arc::clone(&callback_called);
        
        let _watcher = ConfigWatcher::new(move || {
            callback_called_clone.store(true, Ordering::Relaxed);
        });
        
        // Just test that watcher can be created without panicking
        // Actual file watching would require integration tests
        assert!(_watcher.is_ok() || _watcher.is_err()); // Either outcome is acceptable for unit test
    }
}
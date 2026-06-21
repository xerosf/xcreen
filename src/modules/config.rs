use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorConfig {
    pub id: String,
    pub display_device: String,
    pub physical_index: u32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub autostart_enabled: bool,
    pub last_brightness: u32,
    #[serde(default)]
    pub monitors: Vec<MonitorConfig>,
    pub log_level: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            autostart_enabled: false,
            last_brightness: 50,
            monitors: Vec::new(),
            log_level: "warn".to_string(),
        }
    }
}

impl AppConfig {
    pub fn get_config_path() -> Result<PathBuf, String> {
        let exe_path =
            std::env::current_exe().map_err(|e| format!("Failed to get executable path: {e}"))?;
        let exe_dir = exe_path
            .parent()
            .ok_or("Failed to get executable directory")?;
        Ok(exe_dir.join("config.json"))
    }

    pub fn load() -> Result<Self, String> {
        let config_path = Self::get_config_path()?;
        if !config_path.exists() {
            let default_config = Self::default();
            default_config.save()?;
            return Ok(default_config);
        }

        let content = fs::read_to_string(&config_path).map_err(|e| {
            format!(
                "Failed to read config file '{}': {e}",
                config_path.display()
            )
        })?;
        let config = Self::from_json(&content).map_err(|e| {
            format!(
                "Failed to parse config file '{}': {e}\n\nPlease check your JSON syntax or delete the file to regenerate defaults.",
                config_path.display()
            )
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_json(content: &str) -> Result<Self, serde_json::Error> {
        // Serde ignores legacy `brightness_profiles` fields, allowing existing
        // configurations to migrate without keeping profiles in the app model.
        serde_json::from_str(content)
    }

    pub fn save(&self) -> Result<(), String> {
        self.validate()?;
        let config_path = Self::get_config_path()?;
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {e}"))?;
        fs::write(&config_path, json).map_err(|e| format!("Failed to write config file: {e}"))
    }

    pub fn validate(&self) -> Result<(), String> {
        for monitor in &self.monitors {
            if monitor.id.trim().is_empty() {
                return Err("Monitor id cannot be empty".to_string());
            }
        }
        if self.last_brightness > 100 {
            return Err("Last brightness cannot exceed 100%".to_string());
        }
        if !matches!(self.log_level.as_str(), "error" | "warn" | "info" | "debug") {
            return Err(format!(
                "Invalid log level: '{}'. Must be one of: error, warn, info, debug",
                self.log_level
            ));
        }
        Ok(())
    }

    pub fn merge_connected_monitors(
        &mut self,
        monitors: &[crate::modules::monitor::MonitorInfo],
    ) -> bool {
        let original_len = self.monitors.len();
        for monitor in monitors {
            if self
                .monitors
                .iter()
                .any(|configured| configured.id == monitor.id)
            {
                continue;
            }
            self.monitors.push(MonitorConfig {
                id: monitor.id.clone(),
                display_device: monitor.display_device.clone(),
                physical_index: monitor.physical_index,
                name: monitor.name.clone(),
            });
        }
        self.monitors.len() != original_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_config_and_ignores_profiles() {
        let config = AppConfig::from_json(
            r#"{
                "autostart_enabled": true,
                "last_brightness": 42,
                "brightness_profiles": [
                    { "name": "Old", "brightness": 10, "contrast": 20 }
                ],
                "log_level": "debug"
            }"#,
        )
        .unwrap();
        assert!(config.autostart_enabled);
        assert_eq!(config.last_brightness, 42);
        assert!(config.monitors.is_empty());
    }

    #[test]
    fn validates_monitor_ids() {
        let mut config = AppConfig::default();
        config.monitors.push(MonitorConfig {
            id: String::new(),
            display_device: "DISPLAY1".into(),
            physical_index: 0,
            name: "Monitor".into(),
        });
        assert!(config.validate().is_err());
    }
}

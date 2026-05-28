use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrightnessProfile {
    pub name: String,
    pub brightness: u32,
    pub contrast: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorConfig {
    pub id: String,
    pub display_device: String,
    pub physical_index: u32,
    pub name: String,
    pub brightness_profiles: Vec<BrightnessProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub autostart_enabled: bool,
    pub last_brightness: u32,
    pub monitors: Vec<MonitorConfig>,
    pub log_level: String,
    #[serde(skip)]
    migrated_global_profiles: Option<Vec<BrightnessProfile>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyAppConfig {
    autostart_enabled: bool,
    last_brightness: u32,
    brightness_profiles: Vec<BrightnessProfile>,
    log_level: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            autostart_enabled: false,
            last_brightness: 50,
            monitors: Vec::new(),
            log_level: "warn".to_string(),
            migrated_global_profiles: None,
        }
    }
}

impl AppConfig {
    pub fn default_profiles() -> Vec<BrightnessProfile> {
        vec![
            BrightnessProfile {
                name: "Dim".to_string(),
                brightness: 15,
                contrast: 30,
            },
            BrightnessProfile {
                name: "Normal".to_string(),
                brightness: 55,
                contrast: 75,
            },
            BrightnessProfile {
                name: "Max".to_string(),
                brightness: 100,
                contrast: 100,
            },
        ]
    }

    pub fn get_config_path() -> Result<PathBuf, String> {
        let exe_path =
            std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;

        let exe_dir = exe_path
            .parent()
            .ok_or("Failed to get executable directory")?;

        Ok(exe_dir.join("config.json"))
    }

    pub fn load() -> Result<Self, String> {
        let config_path = Self::get_config_path()?;

        if !config_path.exists() {
            eprintln!(
                "Config file not found, creating default configuration at: {}",
                config_path.display()
            );
            let default_config = Self::default();
            default_config.save()?;
            return Ok(default_config);
        }

        let config_content = fs::read_to_string(&config_path).map_err(|e| {
            format!(
                "Failed to read config file '{}': {}",
                config_path.display(),
                e
            )
        })?;

        let config = Self::from_json(&config_content)
            .map_err(|e| format!("Failed to parse config file '{}': {}\n\nPlease check your JSON syntax or delete the file to regenerate defaults.", config_path.display(), e))?;

        config.validate()?;
        Ok(config)
    }

    pub fn from_json(config_content: &str) -> Result<Self, serde_json::Error> {
        if let Ok(config) = serde_json::from_str::<Self>(config_content) {
            return Ok(config);
        }

        let legacy = serde_json::from_str::<LegacyAppConfig>(config_content)?;
        Ok(Self {
            autostart_enabled: legacy.autostart_enabled,
            last_brightness: legacy.last_brightness,
            monitors: Vec::new(),
            log_level: legacy.log_level,
            migrated_global_profiles: Some(legacy.brightness_profiles),
        })
    }

    pub fn save(&self) -> Result<(), String> {
        self.validate()?;

        let config_path = Self::get_config_path()?;

        let config_json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;

        fs::write(&config_path, config_json)
            .map_err(|e| format!("Failed to write config file: {}", e))?;

        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        for monitor in &self.monitors {
            if monitor.id.trim().is_empty() {
                return Err("Monitor id cannot be empty".to_string());
            }
            if monitor.brightness_profiles.is_empty() {
                return Err(format!(
                    "Monitor '{}' must have at least one brightness profile",
                    monitor.name
                ));
            }

            for profile in &monitor.brightness_profiles {
                if profile.brightness > 100 {
                    return Err(format!(
                        "Profile '{}' has invalid brightness: {}",
                        profile.name, profile.brightness
                    ));
                }
                if profile.contrast > 100 {
                    return Err(format!(
                        "Profile '{}' has invalid contrast: {}",
                        profile.name, profile.contrast
                    ));
                }
            }
        }

        if self.last_brightness > 100 {
            return Err("Last brightness cannot exceed 100%".to_string());
        }

        match self.log_level.as_str() {
            "error" | "warn" | "info" | "debug" => {}
            _ => {
                return Err(format!(
                    "Invalid log level: '{}'. Must be one of: error, warn, info, debug",
                    self.log_level
                ));
            }
        }

        Ok(())
    }

    pub fn merge_connected_monitors(
        &mut self,
        monitors: &[crate::modules::monitor::MonitorInfo],
    ) -> bool {
        let original_len = self.monitors.len();
        let had_migrated_profiles = self.migrated_global_profiles.is_some();

        for monitor in monitors {
            if self.monitors.iter().any(|m| m.id == monitor.id) {
                continue;
            }

            let profiles = self
                .migrated_global_profiles
                .clone()
                .filter(|profiles| !profiles.is_empty())
                .unwrap_or_else(Self::default_profiles);

            self.monitors.push(MonitorConfig {
                id: monitor.id.clone(),
                display_device: monitor.display_device.clone(),
                physical_index: monitor.physical_index,
                name: monitor.name.clone(),
                brightness_profiles: profiles,
            });
        }

        if !monitors.is_empty() {
            self.migrated_global_profiles = None;
        }

        self.monitors.len() != original_len || (had_migrated_profiles && !monitors.is_empty())
    }

    pub fn profiles_for_monitor(&self, monitor_id: &str) -> Option<&[BrightnessProfile]> {
        self.monitors
            .iter()
            .find(|monitor| monitor.id == monitor_id)
            .map(|monitor| monitor.brightness_profiles.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_config_without_global_profiles_in_new_shape() {
        let config = AppConfig::from_json(
            r#"{
            "autostart_enabled": true,
            "last_brightness": 42,
            "brightness_profiles": [
                { "name": "refresh", "brightness": 10, "contrast": 20 }
            ],
            "log_level": "debug"
        }"#,
        )
        .unwrap();

        assert!(config.autostart_enabled);
        assert_eq!(config.last_brightness, 42);
        assert!(config.monitors.is_empty());
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.migrated_global_profiles.unwrap()[0].name, "refresh");
    }

    #[test]
    fn validates_per_monitor_profiles_with_duplicate_names() {
        let config = AppConfig {
            autostart_enabled: false,
            last_brightness: 50,
            log_level: "warn".to_string(),
            migrated_global_profiles: None,
            monitors: vec![MonitorConfig {
                id: "display-0-name".to_string(),
                display_device: "\\\\.\\DISPLAY1".to_string(),
                physical_index: 0,
                name: "Monitor".to_string(),
                brightness_profiles: vec![
                    BrightnessProfile {
                        name: "Same".to_string(),
                        brightness: 10,
                        contrast: 20,
                    },
                    BrightnessProfile {
                        name: "Same".to_string(),
                        brightness: 30,
                        contrast: 40,
                    },
                ],
            }],
        };

        assert!(config.validate().is_ok());
    }
}

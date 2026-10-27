use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    pub autostart_enabled: bool,
    pub log_level: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            autostart_enabled: false,
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

        Self::load_existing()
    }

    pub fn load_existing() -> Result<Self, String> {
        let config_path = Self::get_config_path()?;
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
        // Legacy profiles and monitor inventory are ignored.
        serde_json::from_str(content)
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::get_config_path()?)
    }

    fn save_to(&self, config_path: &Path) -> Result<(), String> {
        self.validate()?;
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {e}"))?;
        let temporary = config_path.with_extension(format!("json.{}.tmp", std::process::id()));
        let result = fs::write(&temporary, json).and_then(|()| fs::rename(&temporary, config_path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|e| format!("Failed to write config file: {e}"))
    }

    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.log_level.as_str(), "error" | "warn" | "info" | "debug") {
            return Err(format!(
                "Invalid log level: '{}'. Must be one of: error, warn, info, debug",
                self.log_level
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_replaces_existing_config_and_invalid_settings_leave_it_intact() {
        let directory = std::env::temp_dir().join(format!("xcreen-config-{}", std::process::id()));
        fs::create_dir(&directory).expect("create isolated config fixture");
        let path = directory.join("config.json");
        let mut config = AppConfig::default();
        config.save_to(&path).expect("save defaults");
        config.autostart_enabled = true;
        config.save_to(&path).expect("replace existing file");
        let saved = fs::read_to_string(&path).expect("read replaced config");
        assert!(
            AppConfig::from_json(&saved)
                .expect("valid saved JSON")
                .autostart_enabled
        );
        config.log_level = "invalid".into();
        assert!(config.save_to(&path).is_err());
        assert_eq!(
            fs::read_to_string(&path).expect("read unchanged config"),
            saved
        );
        assert_eq!(fs::read_dir(&directory).expect("list fixture").count(), 1);
        fs::remove_file(path).expect("remove fixture file");
        fs::remove_dir(directory).expect("remove fixture directory");
    }

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
        .expect("legacy configuration must deserialize");
        assert!(config.autostart_enabled);
        assert_eq!(config.log_level, "debug");
    }

    #[test]
    fn rejects_invalid_log_levels() {
        let config = AppConfig {
            log_level: "trace".into(),
            ..AppConfig::default()
        };
        assert!(config.validate().is_err());
    }
}

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
pub struct AppConfig {
    pub autostart_enabled: bool,
    pub last_brightness: u32,
    pub brightness_profiles: Vec<BrightnessProfile>,
    pub log_level: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            autostart_enabled: false,
            last_brightness: 50,
            brightness_profiles: vec![
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
            ],
            log_level: "warn".to_string(),
        }
    }
}

impl AppConfig {
    pub fn get_config_path() -> Result<PathBuf, String> {
        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get executable path: {}", e))?;
        
        let exe_dir = exe_path.parent()
            .ok_or("Failed to get executable directory")?;
        
        Ok(exe_dir.join("config.json"))
    }

    pub fn load() -> Result<Self, String> {
        let config_path = Self::get_config_path()?;
        
        if !config_path.exists() {
            eprintln!("Config file not found, creating default configuration at: {}", config_path.display());
            let default_config = Self::default();
            default_config.save()?;
            return Ok(default_config);
        }

        let config_content = fs::read_to_string(&config_path)
            .map_err(|e| format!("Failed to read config file '{}': {}", config_path.display(), e))?;
        
        let config: Self = serde_json::from_str(&config_content)
            .map_err(|e| {
                format!("Failed to parse config file '{}': {}\n\nPlease check your JSON syntax or delete the file to regenerate defaults.", 
                       config_path.display(), e)
            })?;
        
        config.validate()?;
        Ok(config)
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
        if self.brightness_profiles.is_empty() {
            return Err("At least one brightness profile is required".to_string());
        }
        
        for profile in &self.brightness_profiles {
            if profile.brightness > 100 {
                return Err(format!("Profile '{}' has invalid brightness: {}", profile.name, profile.brightness));
            }
            if profile.contrast > 100 {
                return Err(format!("Profile '{}' has invalid contrast: {}", profile.name, profile.contrast));
            }
        }
        
        
        if self.last_brightness > 100 {
            return Err("Last brightness cannot exceed 100%".to_string());
        }
        
        match self.log_level.as_str() {
            "error" | "warn" | "info" | "debug" => {},
            _ => return Err(format!("Invalid log level: '{}'. Must be one of: error, warn, info, debug", self.log_level))
        }
        
        Ok(())
    }

}

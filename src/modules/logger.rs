use chrono;
use log::{LevelFilter, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::{Write, BufWriter};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

pub struct FileLogger {
    writer: Mutex<BufWriter<File>>,
    level: LevelFilter,
    console_enabled: bool,
}

impl FileLogger {
    pub fn new(log_level: &str) -> Result<Self, String> {
        let level = match log_level.to_lowercase().as_str() {
            "error" => LevelFilter::Error,
            "warn" => LevelFilter::Warn,
            "info" => LevelFilter::Info,
            "debug" => LevelFilter::Debug,
            _ => LevelFilter::Warn,
        };

        let log_path = Self::get_log_path()?;
        
        // Create log file or append to existing
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("Failed to create log file '{}': {}", log_path.display(), e))?;

        let writer = BufWriter::new(file);

        Ok(FileLogger {
            writer: Mutex::new(writer),
            level,
            console_enabled: cfg!(debug_assertions),
        })
    }

    fn get_log_path() -> Result<PathBuf, String> {
        // Get the directory where the executable is located
        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get executable path: {}", e))?;
        
        let exe_dir = exe_path.parent()
            .ok_or("Failed to get executable directory")?;
        
        Ok(exe_dir.join("xcreen.log"))
    }

    fn format_timestamp() -> String {
        match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            Ok(duration) => {
                let secs = duration.as_secs();
                let nanos = duration.subsec_nanos();
                let datetime = chrono::DateTime::from_timestamp(secs as i64, nanos)
                    .unwrap_or_else(chrono::Utc::now);
                datetime.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
            }
            Err(_) => "UNKNOWN_TIME".to_string(),
        }
    }
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let timestamp = Self::format_timestamp();
            let level = record.level();
            let target = record.target();
            let message = record.args();

            let log_line = format!("[{}] {} [{}] {}\n", timestamp, level, target, message);

            // Write to file
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(log_line.as_bytes());
                let _ = writer.flush();
            }

            // Also write to console in debug builds
            if self.console_enabled {
                eprint!("{}", log_line);
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.flush();
        }
    }
}

pub fn init_logger(log_level: &str) -> Result<(), String> {
    let logger = FileLogger::new(log_level)?;
    
    log::set_boxed_logger(Box::new(logger))
        .map_err(|e| format!("Failed to set logger: {}", e))?;
    
    log::set_max_level(match log_level.to_lowercase().as_str() {
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "info" => LevelFilter::Info,
        "debug" => LevelFilter::Debug,
        _ => LevelFilter::Warn,
    });

    // Log initialization message
    log::info!("XCreen logger initialized with level: {}", log_level);
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logger_creation() {
        let logger = FileLogger::new("info");
        assert!(logger.is_ok());
    }

    #[test]
    fn test_log_level_parsing() {
        assert!(FileLogger::new("error").is_ok());
        assert!(FileLogger::new("warn").is_ok());
        assert!(FileLogger::new("info").is_ok());
        assert!(FileLogger::new("debug").is_ok());
        assert!(FileLogger::new("invalid").is_ok()); // Should default to warn
    }
}
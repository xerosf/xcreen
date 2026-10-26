use log::{LevelFilter, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct FileLogger {
    writer: Mutex<BufWriter<File>>,
    console_enabled: bool,
}

fn parse_log_level(log_level: &str) -> LevelFilter {
    match log_level.to_lowercase().as_str() {
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "info" => LevelFilter::Info,
        "debug" => LevelFilter::Debug,
        _ => LevelFilter::Warn,
    }
}

pub fn apply_log_level(log_level: &str) {
    log::set_max_level(parse_log_level(log_level));
}

impl FileLogger {
    pub fn new() -> Result<Self, String> {
        let log_path = Self::get_log_path()?;

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("Failed to create log file '{}': {}", log_path.display(), e))?;

        Ok(FileLogger {
            writer: Mutex::new(BufWriter::new(file)),
            console_enabled: cfg!(debug_assertions),
        })
    }

    fn get_log_path() -> Result<PathBuf, String> {
        let exe_path =
            std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;

        let exe_dir = exe_path
            .parent()
            .ok_or("Failed to get executable directory")?;

        Ok(exe_dir.join("XCreen.log"))
    }
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let log_line = format!(
                "[{}] {} [{}] {}\n",
                record.level(),
                record.target(),
                std::thread::current().name().unwrap_or("main"),
                record.args()
            );

            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(log_line.as_bytes());
                let _ = writer.flush();
            }

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
    let logger = FileLogger::new()?;

    log::set_boxed_logger(Box::new(logger)).map_err(|e| format!("Failed to set logger: {}", e))?;
    apply_log_level(log_level);

    log::info!("XCreen logger initialized (level: {})", log_level);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_log_levels() {
        assert_eq!(parse_log_level("debug"), LevelFilter::Debug);
        assert_eq!(parse_log_level("WARN"), LevelFilter::Warn);
        assert_eq!(parse_log_level("nope"), LevelFilter::Warn);
    }
}

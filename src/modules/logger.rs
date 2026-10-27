use log::{LevelFilter, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_LOG_BYTES: u64 = 1024 * 1024;

fn write_bounded(file: &mut File, entry: &[u8], limit: u64) -> std::io::Result<()> {
    if file.metadata()?.len().saturating_add(entry.len() as u64) > limit {
        file.set_len(0)?;
    }
    file.seek(SeekFrom::End(0))?;
    file.write_all(&entry[..entry.len().min(limit as usize)])
}

pub struct FileLogger {
    writer: Mutex<Option<File>>,
    console_enabled: bool,
}

fn parse_log_level(log_level: &str) -> LevelFilter {
    match log_level.to_lowercase().as_str() {
        "error" => LevelFilter::Error,
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
            .write(true)
            .truncate(false)
            .open(&log_path)
            .map_err(|e| format!("Failed to create log file '{}': {}", log_path.display(), e))?;

        Ok(FileLogger {
            writer: Mutex::new(Some(file)),
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
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let log_line = format!(
                "[{}] {} [{}] {}\n",
                record.level(),
                record.target(),
                std::thread::current().name().unwrap_or("main"),
                record.args()
            );

            if let Ok(mut writer) = self.writer.lock()
                && let Some(file) = writer.as_mut()
            {
                let _ = write_bounded(file, log_line.as_bytes(), MAX_LOG_BYTES);
            }

            if self.console_enabled {
                let _ = std::io::stderr().lock().write_all(log_line.as_bytes());
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut writer) = self.writer.lock()
            && let Some(file) = writer.as_mut()
        {
            let _ = file.flush();
        }
    }
}

pub fn init_logger(log_level: &str) -> Result<(), String> {
    let (logger, file_error) = match FileLogger::new() {
        Ok(logger) => (logger, None),
        Err(err) => (
            FileLogger {
                writer: Mutex::new(None),
                console_enabled: true,
            },
            Some(err),
        ),
    };

    log::set_boxed_logger(Box::new(logger)).map_err(|e| format!("Failed to set logger: {}", e))?;
    apply_log_level(log_level);
    if let Some(err) = file_error {
        tracing::warn!("File logging unavailable: {err}");
    }

    tracing::info!("XCreen logger initialized (level: {})", log_level);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_size_is_bounded_and_new_entries_survive_reset() {
        let path = std::env::temp_dir().join(format!("xcreen-log-{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .expect("create isolated log fixture");
        write_bounded(&mut file, b"old entry\n", 16).expect("write initial entry");
        write_bounded(&mut file, b"latest entry\n", 16).expect("reset full log");
        assert_eq!(std::fs::read(&path).expect("read log"), b"latest entry\n");
        write_bounded(&mut file, &[b'x'; 32], 16).expect("bound oversized entry");
        assert_eq!(file.metadata().expect("log metadata").len(), 16);
        drop(file);
        std::fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn parses_known_log_levels() {
        assert_eq!(parse_log_level("debug"), LevelFilter::Debug);
        assert_eq!(parse_log_level("WARN"), LevelFilter::Warn);
        assert_eq!(parse_log_level("nope"), LevelFilter::Warn);
    }
}

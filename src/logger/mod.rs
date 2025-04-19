use std::fs::{self, File, OpenOptions};
use std::io::{Result as IoResult, Write};
use std::path::{Path, PathBuf};

use crate::errors::{Result, path_error_to_config_error};

/// Log rotation configuration
#[derive(Debug, Clone)]
pub struct LogRotation {
    /// Maximum size of a log file before rotation in bytes
    max_size: u64,
    /// Maximum number of rotated files to keep
    max_files: usize,
}

impl LogRotation {
    /// Create a new log rotation configuration
    pub fn new(max_size: u64) -> Self {
        LogRotation {
            max_size,
            max_files: 5, // Default to 5 rotated files
        }
    }

    /// Create a new log rotation configuration with a custom max_files
    /// Only used in tests
    #[cfg(test)]
    pub fn with_max_files(max_size: u64, max_files: usize) -> Self {
        LogRotation {
            max_size,
            max_files,
        }
    }

    /// Get the maximum size of a log file before rotation
    /// Only used in tests
    #[cfg(test)]
    pub fn max_size(&self) -> u64 {
        self.max_size
    }

    /// Get the maximum number of rotated files to keep
    /// Only used in tests
    #[cfg(test)]
    pub fn _max_files(&self) -> usize {
        self.max_files
    }

    /// Check if a log file needs rotation and perform rotation if needed
    pub fn check_rotation<P: AsRef<Path>>(&self, log_path: P) -> IoResult<()> {
        let path = log_path.as_ref();

        // Check if the file exists
        if !path.exists() {
            return Ok(());
        }

        // Get the file metadata
        let metadata = fs::metadata(path)?;

        // Check if the file is larger than max_size
        if metadata.len() < self.max_size {
            return Ok(());
        }

        // Perform rotation
        self.rotate_log(path)
    }

    /// Rotate a log file
    fn rotate_log<P: AsRef<Path>>(&self, log_path: P) -> IoResult<()> {
        let path = log_path.as_ref();
        let path_str = path.to_string_lossy();

        // Remove the oldest log file if it exists
        let oldest_path = format!("{}.{}", path_str, self.max_files);
        if Path::new(&oldest_path).exists() {
            fs::remove_file(&oldest_path)?;
        }

        // Shift all existing log files
        for i in (1..self.max_files).rev() {
            let src_path = format!("{}.{}", path_str, i);
            let dst_path = format!("{}.{}", path_str, i + 1);

            if Path::new(&src_path).exists() {
                fs::rename(&src_path, &dst_path)?;
            }
        }

        // Rename the current log file to .1
        let backup_path = format!("{}.1", path_str);
        fs::rename(path, &backup_path)?;

        // Create a new empty log file
        File::create(path)?;

        Ok(())
    }
}

/// Logger for handling job output logging with rotation
pub struct Logger {
    /// The path to the stdout log file
    stdout_path: PathBuf,
    /// The path to the stderr log file
    stderr_path: PathBuf,
    /// Log rotation configuration
    rotation: LogRotation,
}

impl Logger {
    /// Create a new logger for the specified job
    pub fn new(stdout_path: PathBuf, stderr_path: PathBuf, rotation: LogRotation) -> Self {
        Logger {
            stdout_path,
            stderr_path,
            rotation,
        }
    }

    /// Write to stdout log file with rotation check
    pub fn write_stdout(&self, data: &[u8]) -> Result<()> {
        self.write_log(&self.stdout_path, data)
    }

    /// Write to stderr log file with rotation check
    pub fn write_stderr(&self, data: &[u8]) -> Result<()> {
        self.write_log(&self.stderr_path, data)
    }

    /// Write to a log file with rotation check
    fn write_log(&self, path: &PathBuf, data: &[u8]) -> Result<()> {
        // Check if the log file needs rotation
        self.rotation
            .check_rotation(path)
            .map_err(|e| path_error_to_config_error(path, e))?;

        // Open the log file for appending
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| path_error_to_config_error(path, e))?;

        // Write the data
        file.write_all(data)
            .map_err(|e| path_error_to_config_error(path, e))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_log_rotation() {
        // Create a temporary directory
        let temp_dir = tempdir().unwrap();
        let log_path = temp_dir.path().join("test.log");

        // Create a log rotation configuration
        let rotation = LogRotation::with_max_files(100, 3); // Small size for testing

        // Create a test file
        {
            let mut file = File::create(&log_path).unwrap();
            file.write_all(b"test data that is larger than 100 bytes...")
                .unwrap();

            // Add more data to exceed max_size
            file.write_all(b"more test data that exceeds the 100 byte limit for this test")
                .unwrap();
        }

        // Rotate the log
        rotation.check_rotation(&log_path).unwrap();

        // Check that the original file was rotated and a new one created
        assert!(log_path.exists());
        assert!(temp_dir.path().join("test.log.1").exists());

        // Check that the new file is empty
        let metadata = fs::metadata(&log_path).unwrap();
        assert_eq!(metadata.len(), 0);
    }
}

use std::path::PathBuf;
use thiserror::Error;

/// Result type for the cron job manager
pub type Result<T> = std::result::Result<T, CronrError>;

/// Errors that can occur in the cron job manager
#[derive(Error, Debug)]
pub enum CronrError {
    /// Failed to read or write config file
    #[error("Failed to read or write config: {0}")]
    ConfigError(String),

    /// Failed to create or access the data directory
    #[error("Failed to access data directory: {0}")]
    DataDirError(String),

    /// Failed to parse a cron expression
    #[error("Invalid cron expression: {0}")]
    InvalidCronExpression(String),

    /// Failed to find a cron job with the given ID
    #[error("Invalid job ID: {0}")]
    InvalidJobId(usize),

    /// Failed to start the daemon process
    #[error("Failed to start daemon: {0}")]
    DaemonStartFailed(String),

    /// Failed to communicate with the daemon process
    #[error("Failed to communicate with daemon: {0}")]
    DaemonCommunicationFailed(String),

    /// Failed to execute a command
    #[error("Command execution failed: {0}")]
    CommandExecutionFailed(String),

    /// Failed to initialize the job manager
    #[error("Failed to initialize job manager: {0}")]
    InitializationError(String),

    /// Failed to rotate logs
    #[error("Log rotation failed: {0}")]
    LogRotationError(String),

    /// Job execution error
    #[error("Job execution error: {0}")]
    JobExecutionError(String),
}

/// Convert a path error to a CronrError
pub fn path_error_to_config_error(path: &PathBuf, err: std::io::Error) -> CronrError {
    CronrError::ConfigError(format!("Error with path {}: {}", path.display(), err))
}

/// Convert an IO error to a CronrError for command execution
pub fn io_error_to_command_error(err: std::io::Error) -> CronrError {
    CronrError::CommandExecutionFailed(format!("IO error: {}", err))
}

/// Convert an IO error to a CronrError for log rotation
pub fn io_error_to_log_rotation_error(err: std::io::Error) -> CronrError {
    CronrError::LogRotationError(format!("IO error: {}", err))
}

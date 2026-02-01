use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time;

use crate::config::Config;
use crate::errors::CronrError;
use crate::errors::Result;
use crate::logger::Logger;

/// A cron job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    /// The command to run
    pub command: String,

    /// The cron expression
    pub cron_expression: String,

    /// Whether the job is enabled
    pub enabled: bool,

    /// The last run time (if any)
    pub last_executed: Option<DateTime<Utc>>,

    /// The next run time (if any)
    pub next_run: Option<DateTime<Utc>>,

    /// Environment variables captured when the job was created
    /// This ensures jobs run with the user's PATH and other important env vars
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl Job {
    /// Create a new job
    pub fn new(command: String, cron_expression: String) -> Result<Self> {
        // Parse the cron expression to validate it
        let schedule = cron_expression
            .parse::<Schedule>()
            .map_err(|e| CronrError::InvalidCronExpression(e.to_string()))?;

        // Calculate the next run time
        let next_run = schedule.upcoming(Utc).next();

        // Capture important environment variables from the user's shell
        // This ensures commands like docker, brew, etc. are found when the job runs
        let mut env = HashMap::new();
        for key in &["PATH", "HOME", "USER", "SHELL", "LANG", "LC_ALL"] {
            if let Ok(value) = std::env::var(key) {
                env.insert(key.to_string(), value);
            }
        }

        Ok(Job {
            command,
            cron_expression,
            enabled: true,
            last_executed: None,
            next_run,
            env,
        })
    }

    /// Get the command
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Set the job as run at the current time
    pub fn set_as_run(&mut self) {
        // Set the last run time to now
        self.last_executed = Some(Utc::now());

        // Recalculate the next run time
        let schedule = self.cron_expression.parse::<Schedule>().unwrap();
        self.next_run = schedule.upcoming(Utc).next();
    }

    /// Get the next run time
    pub fn next_run(&self) -> Option<DateTime<Utc>> {
        self.next_run
    }

    // The following methods are only used in tests
    #[cfg(test)]
    /// Get the cron expression
    pub fn cron_expression(&self) -> &str {
        &self.cron_expression
    }

    #[cfg(test)]
    /// Check if the job is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[cfg(test)]
    /// Enable the job
    pub fn enable(&mut self) {
        self.enabled = true;

        // Recalculate the next run time
        if self.next_run.is_none() {
            let schedule = self.cron_expression.parse::<Schedule>().unwrap();
            self.next_run = schedule.upcoming(Utc).next();
        }
    }

    #[cfg(test)]
    /// Disable the job
    pub fn disable(&mut self) {
        self.enabled = false;
    }

    #[cfg(test)]
    /// Get the last run time
    pub fn last_run(&self) -> Option<DateTime<Utc>> {
        self.last_executed
    }

    #[cfg(test)]
    /// Check if the job is due to run
    pub fn is_due(&self) -> bool {
        // Check if the job is disabled
        if !self.enabled {
            return false;
        }

        // Check if there's a next run time
        if let Some(next_run) = self.next_run {
            // Get the current time
            let now = Utc::now();

            // Check if the next run time is in the past
            return next_run <= now;
        }

        false
    }

    /// Run the job
    pub async fn run(&mut self, config: &Config, job_id: usize) -> Result<()> {
        // Get the stdout and stderr paths
        let stdout_path = config.stdout_log_path(job_id);
        let stderr_path = config.stderr_log_path(job_id);

        // Parse the command to get the program and arguments
        let parts = shell_words::split(&self.command).map_err(|e| {
            CronrError::JobExecutionError(format!("Failed to parse command: {}", e))
        })?;

        if parts.is_empty() {
            return Err(CronrError::JobExecutionError("Empty command".into()));
        }

        // Get the program and arguments
        let program = &parts[0];
        let args = &parts[1..];

        // Create a logger with log rotation
        let logger = Logger::new(
            stdout_path.clone(),
            stderr_path.clone(),
            config.log_rotation().clone(),
        );

        // Create the command
        let mut command = Command::new(program);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Set the captured environment variables (PATH, HOME, etc.)
        // This ensures the job runs with the user's environment from when it was created
        for (key, value) in &self.env {
            command.env(key, value);
        }

        // Run the command
        let child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Err(CronrError::JobExecutionError(format!(
                    "Failed to spawn command: {}",
                    e
                )));
            }
        };

        // Get the output from the command
        let output = match child.wait_with_output() {
            Ok(output) => output,
            Err(e) => {
                return Err(CronrError::JobExecutionError(format!(
                    "Failed to wait for command: {}",
                    e
                )));
            }
        };

        // Write stdout with log rotation
        logger.write_stdout(&output.stdout)?;

        // Write stderr with log rotation
        logger.write_stderr(&output.stderr)?;

        // Mark the job as run
        self.set_as_run();

        Ok(())
    }
}

impl fmt::Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Format the last run time
        let last_run = match self.last_executed {
            Some(time) => time.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => "Never".to_string(),
        };

        // Format the next run time
        let next_run = match self.next_run {
            Some(time) => time.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => "Never".to_string(),
        };

        // Format the job status
        let status = if self.enabled { "Enabled" } else { "Disabled" };

        // Format the job
        write!(
            f,
            "Command: {}\nSchedule: {}\nStatus: {}\nLast Run: {}\nNext Run: {}",
            self.command, self.cron_expression, status, last_run, next_run
        )
    }
}

/// Job executor for running jobs
pub struct JobExecutor {
    /// The job to execute
    job: Job,
}

impl JobExecutor {
    /// Create a new job executor
    pub fn new(job: Job) -> Self {
        JobExecutor { job }
    }

    /// Execute the job according to its schedule
    pub async fn execute_with_schedule(
        &self,
        id: usize,
        config: Config,
        mut stop_signal: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut job = self.job.clone();

        // Calculate the initial sleep time until the next run
        let mut next_run_time = match job.next_run() {
            Some(time) => time,
            None => {
                // No next run time, recalculate
                job.set_as_run();
                match job.next_run() {
                    Some(time) => time,
                    None => {
                        return Err(CronrError::JobExecutionError(
                            "Could not calculate next run time".into(),
                        ));
                    }
                }
            }
        };

        log::info!("Job {} scheduled to run at {}", id, next_run_time);

        loop {
            // Calculate the time until the next run
            let now = Utc::now();

            if next_run_time > now {
                // Sleep until the next run time or until stopped
                let sleep_duration = (next_run_time - now)
                    .to_std()
                    .unwrap_or_else(|_| Duration::from_secs(1));

                log::debug!(
                    "Job {} sleeping for {} seconds",
                    id,
                    sleep_duration.as_secs()
                );

                // Use select to wait for either the timer or the stop signal
                tokio::select! {
                    _ = time::sleep(sleep_duration) => {
                        // Time to execute
                    }
                    _ = stop_signal.changed() => {
                        // Check if we should stop
                        if *stop_signal.borrow() {
                            log::info!("Job {} received stop signal", id);
                            return Ok(());
                        }
                    }
                }
            }

            // Check if current time has passed the next run time
            let now = Utc::now();
            if now >= next_run_time {
                // Time to run the job
                log::info!("Executing job {}: {}", id, job.command());

                // Run the job
                if let Err(e) = job.run(&config, id).await {
                    log::error!("Failed to execute job {}: {}", id, e);
                } else {
                    log::info!("Job {} executed successfully", id);
                }

                // Update the next run time
                next_run_time = match job.next_run() {
                    Some(time) => time,
                    None => {
                        log::error!("Job {} has no next run time after execution", id);
                        return Err(CronrError::JobExecutionError(
                            "Could not calculate next run time".into(),
                        ));
                    }
                };

                log::info!("Job {} next scheduled run: {}", id, next_run_time);
            }

            // Small sleep to prevent CPU spinning if there's a timing issue
            time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_creation() {
        // Create a job
        let job = Job::new("echo test".to_string(), "0 * * * * *".to_string()).unwrap();

        // Check the job
        assert_eq!(job.command(), "echo test");
        assert_eq!(job.cron_expression(), "0 * * * * *");
        assert!(job.is_enabled());
        assert!(job.last_run().is_none());
        assert!(job.next_run().is_some());
    }

    #[test]
    fn test_invalid_cron_expression() {
        // Create a job with an invalid cron expression
        let job = Job::new("echo test".to_string(), "invalid".to_string());

        // Check that the job creation failed
        assert!(job.is_err());
    }

    #[test]
    fn test_job_is_due() {
        // Create a job
        let mut job = Job::new("echo test".to_string(), "0 * * * * *".to_string()).unwrap();

        // Set the next run time to the past
        job.next_run = Some(Utc::now() - chrono::Duration::minutes(1));

        // Check that the job is due
        assert!(job.is_due());

        // Disable the job
        job.disable();

        // Check that the job is not due
        assert!(!job.is_due());
    }
}

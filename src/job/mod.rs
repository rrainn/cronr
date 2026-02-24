use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
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
        // Advance the schedule immediately to prevent tight retry loops on failure.
        // Even if this execution fails, we should wait for the next scheduled time
        // rather than retrying immediately.
        self.set_as_run();

        // Get the stdout and stderr paths
        let stdout_path = config.stdout_log_path(job_id);
        let stderr_path = config.stderr_log_path(job_id);

        // Create a logger with log rotation
        let logger = Logger::new(
            stdout_path.clone(),
            stderr_path.clone(),
            config.log_rotation().clone(),
        );

        // Determine the user's shell (from captured env, or fall back to /bin/sh)
        let shell = self
            .env
            .get("SHELL")
            .map(|s| s.as_str())
            .unwrap_or("/bin/sh");

        // Run the command through a login shell so that profile files
        // (~/.bash_profile, ~/.zprofile, /etc/profile, etc.) are sourced.
        // This ensures PATH and other environment variables are properly set up,
        // even though the daemon process itself runs with a minimal environment.
        log::debug!("Job {} running via login shell: {} -l -c {:?}", job_id, shell, self.command);
        let mut command = Command::new(shell);
        command
            .args(["-l", "-c", &self.command])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Also apply captured env vars as overrides on top of the login shell env
        for (key, value) in &self.env {
            command.env(key, value);
        }

        // Create a new process group for the child process to isolate it from
        // signals sent to the daemon's process group. This prevents signals from
        // interrupting child process system calls (e.g., "Interrupted system call").
        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                // Ignore errors since failing to set process group is not critical
                let _ = nix::unistd::setpgid(
                    nix::unistd::Pid::from_raw(0),
                    nix::unistd::Pid::from_raw(0),
                );
                Ok(())
            });
        }

        // Spawn the child process
        let child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Err(CronrError::JobExecutionError(format!(
                    "Failed to spawn command: {}",
                    e
                )));
            }
        };

        // Wait for the child to complete asynchronously (non-blocking)
        let output = match child.wait_with_output().await {
            Ok(output) => output,
            Err(e) => {
                return Err(CronrError::JobExecutionError(format!(
                    "Failed to wait for command: {}",
                    e
                )));
            }
        };

        // Always write stdout/stderr logs regardless of exit status,
        // so diagnostic output is available for failed jobs too
        logger.write_stdout(&output.stdout)?;
        logger.write_stderr(&output.stderr)?;

        // Check exit status and return an error for non-zero exits
        if output.status.success() {
            log::info!("Job {} command exited successfully", job_id);
            Ok(())
        } else {
            let exit_info = output
                .status
                .code()
                .map_or("signal".to_string(), |c| c.to_string());
            log::warn!(
                "Job {} command exited with status: {}",
                job_id,
                exit_info
            );
            Err(CronrError::JobExecutionError(format!(
                "Command exited with status: {}",
                exit_info
            )))
        }
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

                // Persist the updated job state (next_run, last_executed) to disk
                // so the daemon reload cycle and any restarts see accurate info
                if let Err(e) = config.update_job_state(id, &job) {
                    log::error!("Failed to persist job {} state: {}", id, e);
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
    use crate::config::Config;

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

    /// Test that run() returns an error when the command exits with a non-zero status.
    /// This ensures callers know the job failed so they can log/report it accurately.
    #[tokio::test]
    async fn test_run_returns_error_on_nonzero_exit() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();

        // Create a job with a command that exits with a non-zero status
        let mut job = Job::new("false".to_string(), "0 * * * * *".to_string()).unwrap();

        // Run the job — `false` exits with status 1
        let result = job.run(&config, 0).await;
        assert!(
            result.is_err(),
            "Expected run() to return an error when the command exits with non-zero status"
        );
    }

    /// Test that a failed job run still advances the schedule.
    /// This prevents execute_with_schedule from spinning in a tight retry loop
    /// when a job's command fails to spawn.
    #[tokio::test]
    async fn test_failed_job_run_still_advances_schedule() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();

        // Create a job with a command that will fail to spawn
        let mut job = Job::new(
            "/nonexistent_command_xyz_12345".to_string(),
            "0 * * * * *".to_string(),
        )
        .unwrap();

        // Set next_run to the past (simulating a job that was due to run)
        let past_time = Utc::now() - chrono::Duration::hours(1);
        job.next_run = Some(past_time);

        // Run the job - should fail because the command doesn't exist
        let result = job.run(&config, 0).await;
        assert!(result.is_err(), "Expected job to fail with non-existent command");

        // After the fix: next_run should advance to the future to prevent tight retry loops
        let new_next_run = job.next_run().unwrap();
        assert!(
            new_next_run > Utc::now(),
            "next_run should advance to the future even after a failed run"
        );
    }

    /// Test that jobs run through a login shell and capture stdout correctly.
    /// This verifies the shell-based execution path works end-to-end.
    #[tokio::test]
    async fn test_run_uses_shell_and_captures_output() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();

        // Use a command that only works when interpreted by a shell (echo is a shell builtin)
        let mut job = Job::new("echo hello_from_shell".to_string(), "0 * * * * *".to_string()).unwrap();

        let result = job.run(&config, 0).await;
        assert!(result.is_ok(), "Expected shell command to succeed: {:?}", result);

        // Verify stdout was captured to the log file
        let stdout_log = std::fs::read_to_string(config.stdout_log_path(0)).unwrap();
        assert!(
            stdout_log.contains("hello_from_shell"),
            "Expected stdout log to contain command output, got: {}",
            stdout_log
        );
    }

    /// Test that the SHELL env var is used and a login shell receives captured env overrides.
    #[tokio::test]
    async fn test_run_passes_captured_env_to_shell() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();

        // Create a job that prints a custom env var we'll inject
        let mut job = Job::new("echo $CRONR_TEST_VAR".to_string(), "0 * * * * *".to_string()).unwrap();
        job.env.insert("CRONR_TEST_VAR".to_string(), "test_value_42".to_string());

        let result = job.run(&config, 0).await;
        assert!(result.is_ok(), "Expected command to succeed: {:?}", result);

        // Verify the env var was available inside the command
        let stdout_log = std::fs::read_to_string(config.stdout_log_path(0)).unwrap();
        assert!(
            stdout_log.contains("test_value_42"),
            "Expected stdout to contain env var value, got: {}",
            stdout_log
        );
    }
}

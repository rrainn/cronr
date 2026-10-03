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

/// Normalize a cron expression so it is always in the 6-field format expected
/// by the `cron` crate (seconds, minutes, hours, day-of-month, month, day-of-week).
///
/// Standard 5-field cron expressions omit the leading seconds field.  When only
/// 5 fields are present the function prepends `"0 "` so the job fires at second 0
/// of each matching minute rather than every second.
fn normalize_cron_expression(expr: &str) -> String {
    // Count whitespace-delimited tokens; 5 tokens → standard cron, prepend seconds.
    let field_count = expr.split_whitespace().count();
    if field_count == 5 {
        format!("0 {}", expr)
    } else {
        expr.to_string()
    }
}

/// Parse a cron expression into a [`Schedule`], accepting both the standard
/// 5-field format and the 6-field (seconds-prefixed) format used by the `cron` crate.
fn parse_cron_schedule(expr: &str) -> Result<Schedule> {
    normalize_cron_expression(expr)
        .parse::<Schedule>()
        .map_err(|e| CronrError::InvalidCronExpression(e.to_string()))
}

/// The outcome of the most recent execution of a job
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JobRunStatus {
    /// The command exited with code 0
    Success,
    /// The command exited with a non-zero code or was killed by a signal
    Failed(String),
}

impl std::fmt::Display for JobRunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobRunStatus::Success => write!(f, "Success"),
            JobRunStatus::Failed(info) => write!(f, "Failed ({})", info),
        }
    }
}

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

    /// The status of the last run (if any)
    #[serde(default)]
    pub last_run_status: Option<JobRunStatus>,

    /// Environment variables captured when the job was created
    /// This ensures jobs run with the user's PATH and other important env vars
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl Job {
    /// Create a new job
    pub fn new(command: String, cron_expression: String) -> Result<Self> {
        // Parse the cron expression to validate it (normalizes 5-field expressions)
        let schedule = parse_cron_schedule(&cron_expression)?;

        // Calculate the next run time
        let next_run = schedule.upcoming(Utc).next();

        // Capture environment variables from the calling shell so that jobs can
        // locate binaries (PATH), find Docker's socket (DOCKER_HOST), and access
        // user-level runtime directories without sourcing a login profile.
        // This list intentionally mirrors what cron daemons provide.
        let mut env = HashMap::new();
        for key in &[
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            "LANG",
            "LC_ALL",
            // Docker-related: needed when Docker Desktop or rootless Docker
            // configures a non-default socket path.
            "DOCKER_HOST",
            "DOCKER_CONTEXT",
            // Linux user-level runtime directory used by rootless Docker and
            // other systemd-activated user services.
            "XDG_RUNTIME_DIR",
        ] {
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
            last_run_status: None,
            env,
        })
    }

    /// Get the command
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Update the cron expression and recalculate the next run time
    pub fn reschedule(&mut self, cron_expression: String) -> Result<()> {
        // Validate the new cron expression by parsing it (normalizes 5-field expressions)
        let schedule = parse_cron_schedule(&cron_expression)?;

        // Apply the new expression and recalculate next run
        self.cron_expression = cron_expression;
        self.next_run = schedule.upcoming(Utc).next();

        Ok(())
    }

    /// Set the job as run at the current time
    pub fn set_as_run(&mut self) {
        // Set the last run time to now
        self.last_executed = Some(Utc::now());

        // Recalculate the next run time (normalizes 5-field expressions)
        let schedule = parse_cron_schedule(&self.cron_expression).unwrap();
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

        // Recalculate the next run time (normalizes 5-field expressions)
        if self.next_run.is_none() {
            let schedule = parse_cron_schedule(&self.cron_expression).unwrap();
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

    /// Run the job as a one-off test, streaming output directly to the terminal.
    ///
    /// This mirrors the exact execution environment the daemon uses ([`Job::run`]):
    /// - the same shell (from captured env, no `-l` login flag)
    /// - the same captured environment variables (PATH, DOCKER_HOST, etc.)
    /// - the same working directory (`data_dir`, matching the daemon's cwd)
    /// - the same process-group isolation
    ///
    /// The only intentional differences are that `last_executed`/`next_run` are not
    /// updated, and stdout/stderr are inherited from the calling terminal so the
    /// user can see the output directly.
    pub async fn run_test(&self, config: &Config) -> Result<i32> {
        // Determine the user's shell (from captured env, or fall back to /bin/sh)
        let shell = self
            .env
            .get("SHELL")
            .map(|s| s.as_str())
            .unwrap_or("/bin/sh");

        // Run the command through a non-login shell, inheriting the terminal's
        // stdin/stdout/stderr so that output is visible to the user directly.
        // -l (login shell) is intentionally omitted: login shells source profile
        // files (/etc/profile, ~/.bash_profile, ~/.zprofile) which are designed
        // for interactive sessions and can hang or behave unexpectedly in a
        // daemon context with no controlling terminal.
        // Instead, environment variables are passed explicitly from the captured
        // env, matching exactly what the daemon does in Job::run().
        // Set the working directory to data_dir to match the daemon process,
        // which uses Daemonize::working_directory(&data_dir).
        let mut command = Command::new(shell);
        command
            .args(["-c", &self.command])
            .current_dir(config.data_dir())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        // Apply captured env vars so the shell has PATH, HOME, DOCKER_HOST, etc.
        // matching the exact environment the daemon provides in Job::run().
        for (key, value) in &self.env {
            command.env(key, value);
        }

        // Do NOT call setpgid here (unlike Job::run).
        //
        // In the daemon context, setpgid(0,0) isolates the job's process group
        // from the daemon's group so that signals sent to the daemon don't reach
        // job children.
        //
        // In an interactive `cronr run` context, placing the child in a NEW
        // process group detaches it from the terminal's FOREGROUND process group.
        // Any child that calls tcsetattr on its inherited stdin fd (e.g.
        // `docker compose exec` putting the terminal into raw mode to forward
        // Ctrl+C transparently) receives SIGTTOU from the kernel because it is
        // now a background process modifying terminal settings.  SIGTTOU suspends
        // the process — so docker compose exec hangs before it ever exec's
        // pg_dumpall inside the container, even with the -T flag.
        //
        // Leaving the child in the same process group as cronr keeps it in the
        // terminal's foreground group so terminal ioctls work normally.

        // Spawn the child process
        let mut child = command.spawn().map_err(|e| {
            CronrError::JobExecutionError(format!("Failed to spawn command: {}", e))
        })?;

        // Wait for the child process to complete
        let exit_status = child.wait().await.map_err(|e| {
            CronrError::JobExecutionError(format!("Failed to wait for command: {}", e))
        })?;

        Ok(exit_status.code().unwrap_or(-1))
    }

    /// Run the job
    /// Run the job.
    ///
    /// `stop_signal` allows the caller to interrupt a running job (e.g. on
    /// daemon shutdown or job removal).  When the signal fires the entire
    /// process group (shell + every pipeline child such as `docker exec`,
    /// `pg_dumpall`, `pv`, `gzip`, …) is sent SIGTERM followed by SIGKILL so
    /// no orphaned processes are left behind.
    pub async fn run(
        &mut self,
        config: &Config,
        job_id: usize,
        mut stop_signal: watch::Receiver<bool>,
    ) -> Result<()> {
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

        // Record run-start timestamp and write marker lines to both log files so
        // `cronr logs --timestamps` can show per-run boundaries.
        let run_start_ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        logger.write_stdout_run_header(&run_start_ts)?;
        logger.write_stderr_run_header(&run_start_ts)?;

        // Determine the user's shell (from captured env, or fall back to /bin/sh)
        let shell = self
            .env
            .get("SHELL")
            .map(|s| s.as_str())
            .unwrap_or("/bin/sh");

        // Do NOT use a login shell (-l).  Login shells source /etc/profile and
        // ~/.bash_profile / ~/.zprofile, which are designed for interactive
        // sessions.  In a daemon context (no controlling terminal, no tty):
        //   - profile scripts may call `tput`, `mesg`, or other commands that
        //     query terminal state and block indefinitely;
        //   - on servers with NFS home directories, slow network, or
        //     multi-factor SSH agents, sourcing ~/.profile can stall;
        //   - zsh -l sets up job control which interacts badly with setpgid.
        // Instead, all required environment (PATH, HOME, DOCKER_HOST, etc.) is
        // passed explicitly via the captured env map, matching what traditional
        // cron daemons (vixie cron, dcron) do.
        log::debug!(
            "Job {} running via shell: {} -c {:?}",
            job_id,
            shell,
            self.command
        );
        let mut command = Command::new(shell);
        command
            .args(["-c", &self.command])
            // Clear the inherited environment entirely then re-apply only the
            // captured variables.  This prevents the daemon's minimal env from
            // leaking unexpected values into jobs while ensuring every key the
            // job actually needs (PATH, DOCKER_HOST, XDG_RUNTIME_DIR, …) is set.
            .env_clear()
            // stdin must be /dev/null.  Without it the child shell inherits the
            // daemon's stdin fd (which is /dev/null via daemonize, but in some
            // configurations may be in an undefined state).  docker exec probes
            // its stdin even without -i; an unexpected fd causes it to block
            // before the container process is started.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Apply the captured env vars as the complete environment for the job.
        for (key, value) in &self.env {
            command.env(key, value);
        }

        // Create a new process group for the child process.
        // After setpgid(0, 0) the child's PGID equals its own PID, which lets us
        // later call killpg(child_pid, SIG) to terminate the entire pipeline tree
        // (shell + docker exec + pv + gzip + …) as a single unit on cancellation.
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
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Err(CronrError::JobExecutionError(format!(
                    "Failed to spawn command: {}",
                    e
                )));
            }
        };

        // After setpgid(0, 0) the child's PGID equals its PID.  Capture it now
        // before partially moving `child` so we can signal the whole group later.
        let child_pgid = child.id();
        log::debug!("Job {} spawned shell PID/PGID {:?}", job_id, child_pgid);

        // Drain stdout and stderr into memory in background tasks that run
        // concurrently with the child.  Both pipes must be read continuously:
        // if either pipe buffer fills up (typically 64 KiB) the child blocks
        // mid-write, which stalls the entire pipeline including docker exec,
        // pg_dumpall, and any downstream filters.
        let child_stdout = child.stdout.take().expect("stdout was piped");
        let child_stderr = child.stderr.take().expect("stderr was piped");

        /// Drain an async reader to completion and return the bytes collected.
        async fn drain<R: tokio::io::AsyncRead + Unpin>(reader: R) -> Vec<u8> {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            let _ = tokio::io::BufReader::new(reader)
                .read_to_end(&mut buf)
                .await;
            buf
        }

        let stdout_task = tokio::spawn(drain(child_stdout));
        let stderr_task = tokio::spawn(drain(child_stderr));

        // Wait for the child to exit, or for a stop signal from the daemon.
        // On a stop signal, kill the entire process group so no orphaned children
        // (docker exec, pg_dumpall, gzip, …) are left behind.
        let exit_status = tokio::select! {
            result = child.wait() => {
                result.map_err(|e| CronrError::JobExecutionError(
                    format!("Failed to wait for command: {}", e)
                ))?
            }
            _ = stop_signal.changed() => {
                if *stop_signal.borrow() {
                    log::info!(
                        "Job {} received stop signal; terminating process group {:?}",
                        job_id,
                        child_pgid
                    );
                    #[cfg(unix)]
                    if let Some(pgid) = child_pgid {
                        // SIGTERM first — give processes a chance for graceful shutdown
                        let _ = nix::sys::signal::killpg(
                            nix::unistd::Pid::from_raw(pgid as i32),
                            nix::sys::signal::Signal::SIGTERM,
                        );
                        // Brief grace period before force-killing
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        // SIGKILL as a backstop for anything still alive
                        let _ = nix::sys::signal::killpg(
                            nix::unistd::Pid::from_raw(pgid as i32),
                            nix::sys::signal::Signal::SIGKILL,
                        );
                    }
                    child.wait().await.map_err(|e| CronrError::JobExecutionError(
                        format!("Failed to wait for command after kill: {}", e)
                    ))?
                } else {
                    // Spurious change (value still false); keep waiting normally
                    child.wait().await.map_err(|e| CronrError::JobExecutionError(
                        format!("Failed to wait for command: {}", e)
                    ))?
                }
            }
        };

        // Collect output; the drain tasks finish as soon as the child's pipes
        // are closed by the OS (which happens when the child exits or is killed).
        let stdout_data = stdout_task.await.unwrap_or_default();
        let stderr_data = stderr_task.await.unwrap_or_default();

        // Determine exit info string before branching so it can be used in the
        // run footer regardless of success/failure.
        let exit_info = if exit_status.success() {
            "0".to_string()
        } else {
            exit_status
                .code()
                .map_or("signal".to_string(), |c| c.to_string())
        };

        // Record run end timestamp for the footer marker
        let run_end_ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        // Always write stdout/stderr logs regardless of exit status,
        // so diagnostic output is available for failed jobs too.
        // Each run is bracketed by a header (written before spawning) and a
        // footer (written here) so `cronr logs --timestamps` can demarcate runs.
        logger.write_stdout(&stdout_data)?;
        logger.write_stderr(&stderr_data)?;
        logger.write_stdout_run_footer(&run_end_ts, &exit_info)?;
        logger.write_stderr_run_footer(&run_end_ts, &exit_info)?;

        // Check exit status and return an error for non-zero exits
        if exit_status.success() {
            log::info!("Job {} command exited successfully", job_id);
            // Record successful run status so `cronr info` can display it
            self.last_run_status = Some(JobRunStatus::Success);
            Ok(())
        } else {
            log::warn!("Job {} command exited with status: {}", job_id, exit_info);
            // Record failed run status before returning the error so that
            // `config.update_job_state` (called by the executor) persists it
            self.last_run_status = Some(JobRunStatus::Failed(exit_info.clone()));
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

                // Run the job, passing a clone of the stop signal so the run
                // can kill its process group if the daemon shuts down mid-run.
                if let Err(e) = job.run(&config, id, stop_signal.clone()).await {
                    log::error!("Failed to execute job {}: {}", id, e);
                } else {
                    log::info!("Job {} executed successfully", id);
                }

                // Persist the updated job state (next_run, last_executed) to disk
                // so the daemon reload cycle and any restarts see accurate info
                // A contended cross-process lock must not block the scheduler's async worker.
                let persistence_config = config.clone();
                let completed = job.clone();
                match tokio::task::spawn_blocking(move || {
                    persistence_config.update_job_state(id, &completed)
                })
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => log::error!("Failed to persist job {} state: {}", id, error),
                    Err(error) => log::error!("Job {} persistence task failed: {}", id, error),
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

    /// A standard 5-field cron expression (no seconds field) should be accepted and
    /// treated as if seconds were set to 0, i.e. it fires at second 0 each minute.
    #[test]
    fn test_five_field_cron_accepted_as_zero_seconds() {
        // 5-field expression: "* * * * *" (every minute, no seconds field)
        let job = Job::new("echo test".to_string(), "* * * * *".to_string());
        assert!(
            job.is_ok(),
            "5-field cron expression should be accepted without error"
        );
        let job = job.unwrap();

        // The stored expression should remain exactly as the user typed it
        assert_eq!(job.cron_expression(), "* * * * *");

        // A next run time must be calculated, which proves the schedule was parsed correctly
        assert!(
            job.next_run().is_some(),
            "next_run should be calculated for a 5-field expression"
        );
    }

    /// normalize_cron_expression should prepend "0 " for 5-field expressions and
    /// leave 6-field (and other) expressions unchanged.
    #[test]
    fn test_normalize_cron_expression() {
        assert_eq!(normalize_cron_expression("* * * * *"), "0 * * * * *");
        assert_eq!(normalize_cron_expression("0 * * * * *"), "0 * * * * *");
        assert_eq!(normalize_cron_expression("30 0 12 * * *"), "30 0 12 * * *");
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
        let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let result = job.run(&config, 0, stop_rx).await;
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
        let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let result = job.run(&config, 0, stop_rx).await;
        assert!(
            result.is_err(),
            "Expected job to fail with non-existent command"
        );

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
        let mut job = Job::new(
            "echo hello_from_shell".to_string(),
            "0 * * * * *".to_string(),
        )
        .unwrap();

        let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let result = job.run(&config, 0, stop_rx).await;
        assert!(
            result.is_ok(),
            "Expected shell command to succeed: {:?}",
            result
        );

        // Verify stdout was captured to the log file
        let stdout_log = std::fs::read_to_string(config.stdout_log_path(0)).unwrap();
        assert!(
            stdout_log.contains("hello_from_shell"),
            "Expected stdout log to contain command output, got: {}",
            stdout_log
        );
    }

    /// Test that a freshly created job has no last run status.
    #[test]
    fn test_new_job_has_no_last_run_status() {
        // A brand-new job should not have any run status recorded
        let job = Job::new("echo test".to_string(), "0 * * * * *".to_string()).unwrap();
        assert!(
            job.last_run_status.is_none(),
            "Expected no last_run_status on a new job"
        );
    }

    /// Test that last_run_status is set to Success after a successful run.
    #[tokio::test]
    async fn test_last_run_status_success() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();

        let mut job = Job::new("true".to_string(), "0 * * * * *".to_string()).unwrap();

        // Run the job — `true` always exits with 0
        let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let result = job.run(&config, 0, stop_rx).await;
        assert!(result.is_ok(), "Expected 'true' to succeed: {:?}", result);

        // Status should be recorded as Success
        assert_eq!(
            job.last_run_status,
            Some(JobRunStatus::Success),
            "Expected last_run_status to be Success after a successful run"
        );
    }

    /// Test that last_run_status is set to Failed after a non-zero exit.
    #[tokio::test]
    async fn test_last_run_status_failure() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();

        let mut job = Job::new("false".to_string(), "0 * * * * *".to_string()).unwrap();

        // Run the job — `false` always exits with 1
        let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let result = job.run(&config, 0, stop_rx).await;
        assert!(result.is_err(), "Expected 'false' to fail");

        // Status should be recorded as Failed with the exit code
        match &job.last_run_status {
            Some(JobRunStatus::Failed(info)) => {
                assert_eq!(info, "1", "Expected exit code '1' in Failed status");
            }
            other => panic!(
                "Expected last_run_status to be Failed(\"1\"), got {:?}",
                other
            ),
        }
    }

    /// Test that last_run_status is serialized and deserialized correctly.
    #[test]
    fn test_last_run_status_serde_roundtrip() {
        let mut job = Job::new("echo test".to_string(), "0 * * * * *".to_string()).unwrap();

        // Set a Success status and round-trip through JSON
        job.last_run_status = Some(JobRunStatus::Success);
        let json = serde_json::to_string(&job).unwrap();
        let deserialized: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.last_run_status, Some(JobRunStatus::Success));

        // Set a Failed status and round-trip through JSON
        job.last_run_status = Some(JobRunStatus::Failed("42".to_string()));
        let json = serde_json::to_string(&job).unwrap();
        let deserialized: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.last_run_status,
            Some(JobRunStatus::Failed("42".to_string()))
        );

        // An older job JSON without the field should deserialize with None
        let legacy_json = r#"{"command":"echo test","cron_expression":"0 * * * * *","enabled":true,"last_executed":null,"next_run":null,"env":{}}"#;
        let legacy: Job = serde_json::from_str(legacy_json).unwrap();
        assert!(
            legacy.last_run_status.is_none(),
            "Legacy jobs without last_run_status should default to None"
        );
    }

    /// Test that the SHELL env var is used and a login shell receives captured env overrides.
    #[tokio::test]
    async fn test_run_passes_captured_env_to_shell() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();

        // Create a job that prints a custom env var we'll inject
        let mut job = Job::new(
            "echo $CRONR_TEST_VAR".to_string(),
            "0 * * * * *".to_string(),
        )
        .unwrap();
        job.env
            .insert("CRONR_TEST_VAR".to_string(), "test_value_42".to_string());

        let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let result = job.run(&config, 0, stop_rx).await;
        assert!(result.is_ok(), "Expected command to succeed: {:?}", result);

        // Verify the env var was available inside the command
        let stdout_log = std::fs::read_to_string(config.stdout_log_path(0)).unwrap();
        assert!(
            stdout_log.contains("test_value_42"),
            "Expected stdout to contain env var value, got: {}",
            stdout_log
        );
    }

    /// Test that job stdin is explicitly redirected from /dev/null so commands
    /// that read stdin (like `cat` with no arguments) exit immediately instead of
    /// blocking.  This also covers the docker exec hang: docker exec probes its
    /// stdin even without -i; if the fd is inherited from the daemon in an
    /// undefined state, docker exec blocks before the container process starts.
    #[tokio::test]
    async fn test_run_stdin_is_null() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();

        // `cat` with no arguments reads stdin until EOF.
        // With stdin=/dev/null it receives EOF immediately and exits 0.
        // Without stdin=/dev/null it would block forever.
        let mut job = Job::new("cat".to_string(), "0 * * * * *".to_string()).unwrap();

        let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);

        // A generous timeout: if stdin is not /dev/null the test would hang here
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            job.run(&config, 0, stop_rx),
        )
        .await;

        assert!(
            result.is_ok(),
            "`cat` timed out — stdin was not /dev/null (job would block forever)"
        );
        assert!(
            result.unwrap().is_ok(),
            "`cat` with stdin=/dev/null should exit 0"
        );
    }

    /// Test that a running job's entire process group is killed when a stop
    /// signal is delivered.  Without this, long-running pipelines (docker exec,
    /// pg_dumpall, gzip, …) are left as orphans when the daemon shuts down.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_kills_process_group_on_stop_signal() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();

        // `sleep 300` runs indefinitely; it will only exit when killed
        let mut job = Job::new("sleep 300".to_string(), "0 * * * * *".to_string()).unwrap();

        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

        // Trigger the stop signal after a short delay so the child has time to start
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            stop_tx.send(true).ok();
        });

        // The job should be terminated well within the SIGTERM+SIGKILL grace window
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            job.run(&config, 0, stop_rx),
        )
        .await;

        assert!(
            result.is_ok(),
            "Job did not terminate after the stop signal was sent; \
             process group kill is not working"
        );
    }

    /// Test that the job execution environment is clean: env_clear() removes any
    /// variables inherited by the daemon that were NOT explicitly captured.
    /// This prevents stale or incorrect values (e.g., a daemon-only TERM or
    /// LS_COLORS) from leaking into jobs.
    #[tokio::test]
    async fn test_run_does_not_leak_daemon_env() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();

        // Inject a canary variable into the current process environment.
        // Because job.run() calls env_clear() it must NOT appear in the child.
        // SAFETY: single-threaded test; no other threads read this variable.
        unsafe {
            std::env::set_var("CRONR_DAEMON_CANARY_LEAK", "should_not_appear");
        }

        let mut job = Job::new(
            "echo ${CRONR_DAEMON_CANARY_LEAK:-not_set}".to_string(),
            "0 * * * * *".to_string(),
        )
        .unwrap();
        // Ensure the canary is NOT in the captured env (simulating that it
        // was set after job creation, e.g. by the daemonize step)
        job.env.remove("CRONR_DAEMON_CANARY_LEAK");

        let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let result = job.run(&config, 0, stop_rx).await;
        assert!(result.is_ok(), "Expected command to succeed: {:?}", result);

        let stdout_log = std::fs::read_to_string(config.stdout_log_path(0)).unwrap();
        assert!(
            stdout_log.contains("not_set"),
            "Daemon env variable leaked into job environment; got: {}",
            stdout_log
        );

        unsafe {
            std::env::remove_var("CRONR_DAEMON_CANARY_LEAK");
        }
    }

    /// Test that DOCKER_HOST is captured in the job env when present, so that
    /// jobs using `docker exec` can reach the correct Docker socket even when
    /// the daemon's own environment doesn't have it set.
    #[test]
    fn test_job_captures_docker_host() {
        // SAFETY: single-threaded test; no other threads read this variable.
        unsafe {
            std::env::set_var("DOCKER_HOST", "unix:///run/user/1000/docker.sock");
        }

        let job = Job::new("echo test".to_string(), "0 * * * * *".to_string()).unwrap();
        assert_eq!(
            job.env.get("DOCKER_HOST").map(|s| s.as_str()),
            Some("unix:///run/user/1000/docker.sock"),
            "DOCKER_HOST must be captured so docker exec works in daemon jobs"
        );

        unsafe {
            std::env::remove_var("DOCKER_HOST");
        }
    }
}

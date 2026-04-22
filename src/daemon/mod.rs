use daemonize::Daemonize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::Command;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Duration;

use crate::config::JobManager;
use crate::errors::{CronrError, Result, path_error_to_config_error};
use crate::job::{Job, JobExecutor};

/// The daemon process manager
pub struct Daemon {
    /// The data directory
    data_dir: PathBuf,
}

impl Daemon {
    /// Create a new daemon with the given data directory
    pub fn new(data_dir: PathBuf) -> Self {
        Daemon { data_dir }
    }

    /// Start the daemon
    pub fn start(&self) -> Result<()> {
        // Check if the daemon is already running
        if self.is_running() {
            return Err(CronrError::DaemonStartFailed(
                "Daemon is already running".into(),
            ));
        }

        // Create the pidfile path
        let pid_file = self.pid_file();

        // Create the logfile paths
        let stdout_file = self.data_dir.join("daemon.log");

        // Create the necessary files
        let stdout =
            File::create(&stdout_file).map_err(|e| path_error_to_config_error(&stdout_file, e))?;

        // Log that we're going to start the daemon
        log::info!("Starting daemon process");

        // Create the daemonize configuration
        let daemonize = Daemonize::new()
            .pid_file(pid_file)
            .working_directory(&self.data_dir)
            .stdout(
                stdout
                    .try_clone()
                    .map_err(|e| path_error_to_config_error(&stdout_file, e))?,
            )
            .stderr(stdout);

        // Start the daemon
        match daemonize.start() {
            Ok(_) => {
                // We're in the daemon process
                // Run the daemon internal command
                let exe = std::env::current_exe().map_err(|e| {
                    CronrError::DaemonStartFailed(format!("Failed to get executable path: {}", e))
                })?;

                let status = Command::new(exe)
                    .arg("daemon-internal")
                    .status()
                    .map_err(|e| {
                        CronrError::DaemonStartFailed(format!(
                            "Failed to start daemon process: {}",
                            e
                        ))
                    })?;

                // This should not be reached in the daemon process
                if !status.success() {
                    return Err(CronrError::DaemonStartFailed(format!(
                        "Daemon process exited with status {}",
                        status.code().unwrap_or(-1)
                    )));
                }

                std::process::exit(0);
            }
            Err(e) => {
                // Failed to start the daemon
                return Err(CronrError::DaemonStartFailed(format!(
                    "Failed to daemonize: {}",
                    e
                )));
            }
        }
    }

    /// Stop the daemon
    pub fn stop(&self) -> Result<()> {
        // Check if the daemon is running
        if !self.is_running() {
            return Err(CronrError::DaemonStartFailed(
                "Daemon is not running".into(),
            ));
        }

        // Get the PID
        let pid_file = self.pid_file();
        let pid_str =
            fs::read_to_string(&pid_file).map_err(|e| path_error_to_config_error(&pid_file, e))?;

        let pid = pid_str
            .trim()
            .parse::<u32>()
            .map_err(|e| CronrError::DaemonStartFailed(format!("Failed to parse PID: {}", e)))?;

        // Kill the process
        // Different signals for different platforms
        #[cfg(target_os = "linux")]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;

            kill(Pid::from_raw(pid as i32), Signal::SIGTERM).map_err(|e| {
                CronrError::DaemonStartFailed(format!("Failed to kill daemon: {}", e))
            })?;
        }

        #[cfg(target_os = "macos")]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;

            kill(Pid::from_raw(pid as i32), Signal::SIGTERM).map_err(|e| {
                CronrError::DaemonStartFailed(format!("Failed to kill daemon: {}", e))
            })?;
        }

        #[cfg(target_os = "windows")]
        {
            let status = Command::new("taskkill")
                .args(&["/F", "/PID", &pid.to_string()])
                .status()
                .map_err(|e| {
                    CronrError::DaemonStartFailed(format!("Failed to kill daemon: {}", e))
                })?;

            if !status.success() {
                return Err(CronrError::DaemonStartFailed(format!(
                    "Failed to kill daemon, taskkill returned {}",
                    status.code().unwrap_or(-1)
                )));
            }
        }

        // Remove the PID file
        fs::remove_file(&pid_file).map_err(|e| path_error_to_config_error(&pid_file, e))?;

        Ok(())
    }

    /// Check if the daemon is running
    pub fn is_running(&self) -> bool {
        // Check if the PID file exists
        let pid_file = self.pid_file();
        if !pid_file.exists() {
            return false;
        }

        // Read the PID
        let pid_str = match fs::read_to_string(&pid_file) {
            Ok(s) => s,
            Err(_) => return false,
        };

        let pid = match pid_str.trim().parse::<u32>() {
            Ok(p) => p,
            Err(_) => {
                // Invalid PID file, clean it up
                let _ = fs::remove_file(&pid_file);
                return false;
            }
        };

        // Check if the process is running AND is actually the cronr daemon.
        // Only checking liveness (via SIGCONT) is insufficient: after the daemon exits the OS
        // can reuse its PID for an entirely different process, which would cause a false
        // positive and prevent the daemon from ever being restarted.
        #[cfg(target_os = "linux")]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;

            match kill(Pid::from_raw(pid as i32), Signal::SIGCONT) {
                Ok(_) => {
                    // Process is alive — verify it is cronr, not a PID-reuse impostor
                    if !Self::is_cronr_process(pid) {
                        let _ = fs::remove_file(&pid_file);
                        false
                    } else {
                        true
                    }
                }
                Err(_) => {
                    // Process is not running, clean up the PID file
                    let _ = fs::remove_file(&pid_file);
                    false
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;

            match kill(Pid::from_raw(pid as i32), Signal::SIGCONT) {
                Ok(_) => {
                    // Process is alive — verify it is cronr, not a PID-reuse impostor
                    if !Self::is_cronr_process(pid) {
                        let _ = fs::remove_file(&pid_file);
                        false
                    } else {
                        true
                    }
                }
                Err(_) => {
                    // Process is not running, clean up the PID file
                    let _ = fs::remove_file(&pid_file);
                    false
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            let output = match Command::new("tasklist")
                .args(&["/FI", &format!("PID eq {}", pid), "/FI", "IMAGENAME eq cronr.exe"])
                .output()
            {
                Ok(o) => o,
                Err(_) => return false,
            };

            if !output.status.success() {
                return false;
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains(&format!("{}", pid)) {
                true
            } else {
                // Process is not running or is not cronr, clean up the PID file
                let _ = fs::remove_file(&pid_file);
                false
            }
        }
    }

    /// Return true if the process with the given PID has "cronr" in its executable name.
    /// Used to guard against PID reuse after an unclean daemon shutdown.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn is_cronr_process(pid: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            // /proc/<pid>/comm holds the executable name (up to 15 chars, no path)
            let comm_path = format!("/proc/{}/comm", pid);
            match fs::read_to_string(&comm_path) {
                Ok(comm) => comm.trim().contains("cronr"),
                Err(_) => false,
            }
        }

        #[cfg(target_os = "macos")]
        {
            // `ps -p <pid> -o comm=` prints just the executable basename
            let output = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "comm="])
                .output();
            match output {
                Ok(out) if out.status.success() => {
                    let name = String::from_utf8_lossy(&out.stdout);
                    name.trim().contains("cronr")
                }
                _ => false,
            }
        }
    }

    /// Get the PID file path
    fn pid_file(&self) -> PathBuf {
        self.data_dir.join("cronr.pid")
    }

    /// Register for system startup
    pub fn register_for_startup(&self) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            // Linux - handled by systemd script
            log::info!("System startup registration is handled by the systemd service file");
        }

        #[cfg(target_os = "macos")]
        {
            // macOS - handled by LaunchAgent
            log::info!("System startup registration is handled by the LaunchAgent plist");
        }

        #[cfg(target_os = "windows")]
        {
            // Get the path to the executable
            let exe = std::env::current_exe().map_err(|e| {
                CronrError::InitializationError(format!("Failed to get executable path: {}", e))
            })?;

            // Register with the Task Scheduler
            let status = Command::new("schtasks")
                .args(&[
                    "/create",
                    "/tn",
                    "Cronr Service",
                    "/sc",
                    "onstart",
                    "/ru",
                    "SYSTEM",
                    "/tr",
                    &format!("\"{}\" start", exe.display()),
                    "/f",
                ])
                .status()
                .map_err(|e| {
                    CronrError::InitializationError(format!(
                        "Failed to register with Task Scheduler: {}",
                        e
                    ))
                })?;

            if !status.success() {
                return Err(CronrError::InitializationError(format!(
                    "Failed to register with Task Scheduler, exit code: {}",
                    status.code().unwrap_or(-1)
                )));
            }

            log::info!("Successfully registered for startup via Windows Task Scheduler");
        }

        Ok(())
    }
}

/// The daemon runner
pub struct DaemonRunner {
    /// The job manager
    job_manager: JobManager,

    /// The running job handles
    job_handles: HashMap<usize, JoinHandle<Result<()>>>,

    /// The job stop signals
    job_stop_signals: HashMap<usize, watch::Sender<bool>>,
}

impl DaemonRunner {
    /// Create a new daemon runner
    pub async fn new() -> Result<Self> {
        // Create the job manager
        let job_manager = JobManager::new().await?;

        Ok(DaemonRunner {
            job_manager,
            job_handles: HashMap::new(),
            job_stop_signals: HashMap::new(),
        })
    }

    /// Create a new daemon runner with existing JobManager
    pub async fn with_job_manager(job_manager: JobManager) -> Result<Self> {
        Ok(DaemonRunner {
            job_manager,
            job_handles: HashMap::new(),
            job_stop_signals: HashMap::new(),
        })
    }

    /// Load an existing daemon runner
    pub async fn load() -> Result<Self> {
        // Load existing job manager (instead of creating a new one)
        let job_manager = JobManager::load().await?;

        log::info!("Daemon loaded from existing configuration");

        Ok(DaemonRunner {
            job_manager,
            job_handles: HashMap::new(),
            job_stop_signals: HashMap::new(),
        })
    }

    /// Run the daemon, dynamically reloading jobs
    pub async fn run(&mut self) -> Result<()> {
        // Log startup
        log::info!("Daemon starting up");

        loop {
            // Reload job manager from disk to pick up external changes
            self.job_manager = JobManager::load().await?;
            // Get all jobs from the freshly loaded state
            let jobs = self.job_manager.get_all_jobs().await;
            log::info!("Loaded {} jobs", jobs.len());
            // Debug each job's schedule details
            for (id, job) in &jobs {
                log::debug!(
                    "Job {} details: command={}, enabled={}, next_run={:?}, last_executed={:?}, env_vars={}",
                    id,
                    job.command(),
                    job.enabled,
                    job.next_run(),
                    job.last_executed,
                    job.env.len()
                );
            }

            // Determine jobs to stop: removed or disabled
            let loaded_ids: std::collections::HashSet<usize> = jobs.keys().cloned().collect();
            let running_ids: Vec<usize> = self.job_handles.keys().cloned().collect();
            // Stop jobs that are no longer present
            for id in running_ids {
                if !loaded_ids.contains(&id) {
                    log::info!("Stopping removed job {}", id);
                    self.stop_job(id).await?;
                }
            }
            // Stop jobs that have been disabled
            for (id, job) in &jobs {
                if !job.enabled && self.job_handles.contains_key(id) {
                    log::info!("Stopping disabled job {}", id);
                    self.stop_job(*id).await?;
                }
            }

            // Detect and clean up completed job executor tasks.
            // If a job's executor task has finished (e.g., due to an unrecoverable error),
            // remove it from running jobs so it can be restarted on the next cycle.
            let completed_ids: Vec<usize> = self
                .job_handles
                .iter()
                .filter(|(_, handle)| handle.is_finished())
                .map(|(id, _)| *id)
                .collect();
            for id in completed_ids {
                log::warn!(
                    "Job {} executor task completed unexpectedly, will restart",
                    id
                );
                self.job_handles.remove(&id);
                self.job_stop_signals.remove(&id);
            }

            // Start any new enabled jobs not yet running
            for (id, job) in &jobs {
                if job.enabled && !self.job_handles.contains_key(id) {
                    // Start new job
                    log::info!("{}", &format!("Starting job {}: {}", id, job.command()));
                    self.start_job(*id, job.clone()).await?;
                }
            }

            // Wait for shutdown or next reload interval
            tokio::select! {
                _ = self.wait_for_signal() => {
                    log::info!("Shutdown signal received");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(30)) => {
                    // continue to next cycle
                }
            }
        }

        // Stop all jobs on shutdown
        self.stop_all_jobs().await?;
        Ok(())
    }

    /// Start a job
    pub async fn start_job(&mut self, id: usize, job: Job) -> Result<()> {
        // Check if job is already running
        if self.job_handles.contains_key(&id) {
            log::warn!("Job {} is already running, stopping it first", id);
            self.stop_job(id).await?;
        }

        // Create a stop signal channel
        let (stop_tx, stop_rx) = watch::channel(false);

        // Clone the job manager config
        let config = self.job_manager.config().clone();

        // Start the job in a separate task
        let job_clone = job.clone();
        let handle = tokio::spawn(async move {
            // Create job executor
            let executor = JobExecutor::new(job_clone);

            // Run the job
            executor.execute_with_schedule(id, config, stop_rx).await
        });

        // Store the handle and stop signal
        self.job_handles.insert(id, handle);
        self.job_stop_signals.insert(id, stop_tx);

        Ok(())
    }

    /// Stop a job
    pub async fn stop_job(&mut self, id: usize) -> Result<()> {
        // Get the stop signal
        let stop_tx = match self.job_stop_signals.remove(&id) {
            Some(tx) => tx,
            None => {
                log::warn!("No stop signal for job {}, it may not be running", id);
                return Ok(());
            }
        };

        // Get the job handle
        let handle = match self.job_handles.remove(&id) {
            Some(h) => h,
            None => {
                log::warn!("No handle for job {}, it may not be running", id);
                return Ok(());
            }
        };

        // Send the stop signal
        stop_tx.send(true).map_err(|_| {
            CronrError::CommandExecutionFailed(format!("Failed to send stop signal to job {}", id))
        })?;

        // Wait for the job to stop
        handle.await.map_err(|e| {
            CronrError::CommandExecutionFailed(format!("Failed to join job task: {}", e))
        })??;

        Ok(())
    }

    /// Stop all jobs
    pub async fn stop_all_jobs(&mut self) -> Result<()> {
        // Get all job IDs
        let job_ids: Vec<usize> = self.job_handles.keys().cloned().collect();

        log::info!("Stopping all jobs ({})", job_ids.len());

        // Stop all jobs
        for id in job_ids {
            if let Err(e) = self.stop_job(id).await {
                log::error!("Failed to stop job {}: {}", id, e);
            } else {
                log::info!("Stopped job {}", id);
            }
        }

        Ok(())
    }

    /// Wait for a termination signal
    async fn wait_for_signal(&self) -> Result<()> {
        // Set up the signal handler
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let mut sigterm = signal(SignalKind::terminate()).map_err(|e| {
                CronrError::InitializationError(format!("Failed to set up signal handler: {}", e))
            })?;

            let mut sigint = signal(SignalKind::interrupt()).map_err(|e| {
                CronrError::InitializationError(format!("Failed to set up signal handler: {}", e))
            })?;

            tokio::select! {
                _ = sigterm.recv() => {
                    log::info!("Received SIGTERM, shutting down");
                }
                _ = sigint.recv() => {
                    log::info!("Received SIGINT, shutting down");
                }
            }
        }

        #[cfg(windows)]
        {
            use tokio::signal::windows::{ctrl_break, ctrl_c};

            let mut ctrlc = ctrl_c().map_err(|e| {
                CronrError::InitializationError(format!("Failed to set up signal handler: {}", e))
            })?;

            let mut ctrlbreak = ctrl_break().map_err(|e| {
                CronrError::InitializationError(format!("Failed to set up signal handler: {}", e))
            })?;

            tokio::select! {
                _ = ctrlc.recv() => {
                    log::info!("Received Ctrl+C, shutting down");
                }
                _ = ctrlbreak.recv() => {
                    log::info!("Received Ctrl+Break, shutting down");
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_daemon_pid_file() {
        // Create a temporary directory
        let temp_dir = tempdir().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // Create a daemon
        let daemon = Daemon::new(data_dir.clone());

        // Check that the PID file is in the data directory
        assert_eq!(daemon.pid_file(), data_dir.join("cronr.pid"));
    }

    /// Regression test: `is_running()` must return false when the stored PID belongs to a
    /// non-cronr process. Before the fix, `is_running()` only checked whether *any* process
    /// with that PID was alive (via SIGCONT), which caused false positives whenever the OS
    /// reused a dead daemon's PID for a completely different program.
    #[cfg(unix)]
    #[test]
    fn test_is_running_false_for_non_cronr_process() {
        let temp_dir = tempdir().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // Spawn `sleep 30` — a process that is definitely not "cronr".
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("failed to spawn sleep");
        let pid = child.id();

        // Write its PID into the daemon PID file, simulating a reused PID.
        let pid_file = data_dir.join("cronr.pid");
        std::fs::write(&pid_file, pid.to_string()).unwrap();

        let daemon = Daemon::new(data_dir.clone());

        // is_running() should return false: the process exists but is not cronr.
        let result = daemon.is_running();

        // Always clean up the child process regardless of the assertion outcome.
        child.kill().ok();
        child.wait().ok();

        assert!(
            !result,
            "is_running() returned true for a non-cronr process (PID {}); \
             stale/reused PIDs must not be treated as a live daemon",
            pid
        );
    }
}

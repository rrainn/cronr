use clap::{Parser, Subcommand};
use std::io::{self, Read, Seek, SeekFrom, Write};
use tokio::runtime::Runtime;

use crate::config::JobManager;
use crate::daemon::Daemon;
use crate::errors::{CronrError, Result};

/// Command-line arguments for the cron manager
#[derive(Parser, Debug)]
#[clap(author, version, about)]
pub struct Cli {
    /// The subcommand to run
    #[clap(subcommand)]
    pub command: Option<Commands>,
}

/// Subcommands for the cron manager
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Create a new cron job
    #[clap(name = "create")]
    Create {
        /// The command to execute
        command: String,

        /// The cron expression (e.g., "0 * * * *" for every hour)
        #[clap(name = "schedule")]
        cron_expression: String,
    },

    /// List all cron jobs
    #[clap(name = "ls")]
    List,

    /// Stop a cron job
    #[clap(name = "stop")]
    Stop {
        /// The ID of the job to stop
        id: usize,
    },

    /// Show version information
    #[clap(name = "version")]
    Version,

    /// Start the daemon
    #[clap(name = "start", hide = true)]
    Start,

    /// Stop the daemon
    #[clap(name = "daemon-stop", hide = true)]
    DaemonStop,

    /// Run a job immediately as a one-off test (does not affect the schedule)
    #[clap(name = "run")]
    Run {
        /// The ID of the job to run
        id: usize,
    },

    /// Check the status of the daemon and tool
    #[clap(name = "status")]
    Status,

    /// Show details about a specific job
    #[clap(name = "info")]
    Info {
        /// The ID of the job to inspect
        id: usize,
    },

    /// Edit the cron schedule of an existing job
    #[clap(name = "edit")]
    Edit {
        /// The ID of the job to edit
        id: usize,

        /// The new cron expression (e.g., "0 0 * * * *" for every hour on the hour)
        #[clap(name = "schedule")]
        cron_expression: String,
    },

    /// Internal command used by the daemon process
    #[clap(name = "daemon-internal", hide = true)]
    DaemonInternal,

    /// Show logs for a specific job
    #[clap(name = "logs")]
    Logs {
        /// The ID of the job whose logs to display
        id: usize,

        /// Show stderr log instead of stdout
        #[clap(long, short = 'e')]
        stderr: bool,

        /// Limit output to the last N lines (omit to show all)
        #[clap(long, short = 'n', value_name = "N")]
        lines: Option<usize>,

        /// Stream (follow) new log output as it is written, like tail -f
        #[clap(long, short = 'f')]
        follow: bool,

        /// Include run-boundary markers that show the timestamp of each run
        #[clap(long, short = 't')]
        timestamps: bool,
    },
}

/// Run the command-line interface
pub fn run(cli: Cli) -> Result<()> {
    // Handle commands
    match cli.command {
        Some(Commands::Create {
            command,
            cron_expression,
        }) => create_job(command, cron_expression),
        Some(Commands::List) => list_jobs(),
        Some(Commands::Stop { id }) => stop_job(id),
        Some(Commands::Version) => print_version(),
        Some(Commands::Run { id }) => run_job_test(id),
        Some(Commands::Start) => start_daemon(),
        Some(Commands::DaemonStop) => stop_daemon(),
        Some(Commands::Status) => check_daemon_status(),
        Some(Commands::Info { id }) => info_job(id),
        Some(Commands::Edit { id, cron_expression }) => edit_job(id, cron_expression),
        Some(Commands::DaemonInternal) => run_daemon_internal(),
        Some(Commands::Logs {
            id,
            stderr,
            lines,
            follow,
            timestamps,
        }) => show_logs(id, stderr, lines, follow, timestamps),
        None => {
            // If no command is provided, show help
            println!("cronr: cron task manager");
            println!("Run 'cronr --help' for usage information");
            Ok(())
        }
    }
}

/// Print version information
fn print_version() -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    println!("cronr {}", version);
    Ok(())
}

/// Create a new cron job
fn create_job(command: String, cron_expression: String) -> Result<()> {
    // Create the runtime
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;

    // Run the async block
    let data_dir = rt.block_on(async {
        // Create the job manager
        let job_manager = JobManager::new().await?;

        // Add the job
        let id = job_manager
            .add_job(command.clone(), cron_expression.clone())
            .await?;

        // Print the job ID
        println!("Added job {} with schedule '{}'", id, cron_expression);
        println!("Command: {}", command);

        Ok::<_, CronrError>(job_manager.config().data_dir().to_path_buf())
    })?;

    // Fork only after runtime workers have stopped; inherited Tokio state is unsafe in a daemon.
    drop(rt);
    let daemon = Daemon::new(data_dir);
    if !daemon.is_running() {
        daemon.start()?;
        println!("Started daemon for job execution");
    }
    Ok(())
}

/// List all cron jobs
fn list_jobs() -> Result<()> {
    // Create the runtime
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;

    // Run the async block
    rt.block_on(async {
        // Load the job manager from existing configuration
        let job_manager = JobManager::load().await?;

        // Get all jobs
        let jobs = job_manager.get_all_jobs().await;

        // Check if there are no jobs
        if jobs.is_empty() {
            println!("No cron jobs found.");
            return Ok(());
        }

        // Print the jobs
        println!("ID | Schedule       | Command");
        println!("---|---------------|--------");

        let mut sorted_jobs: Vec<_> = jobs.iter().collect();
        sorted_jobs.sort_by_key(|&(id, _)| *id);

        for (id, job) in sorted_jobs {
            println!("{:2} | {:<13} | {}", id, job.cron_expression, job.command);
        }

        // Return success
        Ok(())
    })
}

/// Stop a cron job
fn stop_job(id: usize) -> Result<()> {
    // Create the runtime
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;

    // Run the async block
    rt.block_on(async {
        // Load the job manager from existing configuration
        let job_manager = JobManager::load().await?;

        // Get the job (to display information before removing)
        let job = job_manager.get_job(id).await?;

        // Remove the job
        job_manager.remove_job(id).await?;

        // Print the job ID
        println!("Stopped job {} with schedule '{}'", id, job.cron_expression);
        println!("Command: {}", job.command);

        // Return success
        Ok(())
    })
}

/// Start the daemon
fn start_daemon() -> Result<()> {
    // Create the runtime
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;

    let data_dir = rt.block_on(async {
        // Initialize missing storage while preserving any existing parse or I/O error.
        let job_manager = JobManager::new().await?;
        Ok::<_, CronrError>(job_manager.config().data_dir().to_path_buf())
    })?;

    // Daemonization must not inherit active runtime workers or their locks.
    drop(rt);
    let daemon = Daemon::new(data_dir);
    if daemon.is_running() {
        println!("Daemon is already running.");
        return Ok(());
    }
    daemon.start()?;
    println!("Started daemon.");
    Ok(())
}

/// Stop the daemon
fn stop_daemon() -> Result<()> {
    // Create the runtime
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;

    // Run the async block
    rt.block_on(async {
        // Load the job manager from existing configuration
        let job_manager = JobManager::load().await?;

        // Create the daemon
        let daemon = Daemon::new(job_manager.config().data_dir().to_path_buf());

        // Check if the daemon is running
        if !daemon.is_running() {
            println!("Daemon is not running.");
            return Ok(());
        }

        // Stop the daemon
        daemon.stop()?;

        // Print the status
        println!("Stopped daemon.");

        // Return success
        Ok(())
    })
}

/// Check the status of the daemon and tool
fn check_daemon_status() -> Result<()> {
    // Create the runtime
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;

    // Run the async block
    rt.block_on(async {
        // Initialize missing storage while preserving any existing parse or I/O error.
        let job_manager = JobManager::new().await?;

        // Get active job count
        let active_count = job_manager.get_all_jobs().await.len();

        // Print version
        println!("cronr version: {}", env!("CARGO_PKG_VERSION"));

        // Print number of active jobs
        println!("Active jobs: {}", active_count);

        // Create the daemon
        let daemon = Daemon::new(job_manager.config().data_dir().to_path_buf());

        // Print daemon status
        if daemon.is_running() {
            println!("Daemon is running.");
        } else {
            println!("Daemon is not running.");
        }

        // Return success
        Ok(())
    })
}

/// Run a job once immediately as a test, streaming output to the terminal.
///
/// The execution environment matches the daemon as closely as possible:
/// - same login shell and captured env-var overrides
/// - same working directory (`data_dir`, matching the cwd set by `daemonize`)
/// - same process-group isolation
///
/// The job's `last_executed` and `next_run` fields are not modified.
fn run_job_test(id: usize) -> Result<()> {
    // Create the async runtime
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;

    // Run the async block
    rt.block_on(async {
        // Load the job manager from existing configuration
        let job_manager = JobManager::load().await?;

        // Retrieve the job by ID so we can display its command before running
        let job = job_manager.get_job(id).await?;

        println!("Running job {} one-off test:", id);
        println!("  Command:  {}", job.command);
        println!("  Schedule: {}", job.cron_expression);
        println!("{}", "─".repeat(40));

        // Execute the job using the same config the daemon would use, so the
        // working directory and paths are identical to a scheduled run.
        let exit_code = job.run_test(job_manager.config()).await?;

        println!("{}", "─".repeat(40));

        if exit_code == 0 {
            println!("Job {} completed successfully (exit code 0).", id);
        } else {
            println!("Job {} exited with code {}.", id, exit_code);
        }

        Ok(())
    })
}

/// Show detailed information about a specific cron job
fn info_job(id: usize) -> Result<()> {
    // Create the async runtime
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;

    // Run the async block
    rt.block_on(async {
        // Load the job manager from existing configuration
        let job_manager = JobManager::load().await?;

        // Retrieve the job by ID
        let job = job_manager.get_job(id).await?;

        // Format the last run time
        let last_run = job
            .last_executed
            .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "Never".to_string());

        // Format the next run time
        let next_run = job
            .next_run
            .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "N/A".to_string());

        // Format the status of the last run
        let last_run_status = job
            .last_run_status
            .as_ref()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "N/A".to_string());

        // Print the job details
        println!("Job {}", id);
        println!("{}", "─".repeat(40));
        println!("  ID:              {}", id);
        println!("  Command:         {}", job.command);
        println!("  Schedule:        {}", job.cron_expression);
        println!("  Last Run:        {}", last_run);
        println!("  Next Run:        {}", next_run);
        println!("  Last Run Status: {}", last_run_status);

        Ok(())
    })
}

/// Edit the cron schedule of an existing job
fn edit_job(id: usize, cron_expression: String) -> Result<()> {
    // Create the async runtime
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;

    // Run the async block
    rt.block_on(async {
        // Load the job manager from existing configuration
        let job_manager = JobManager::load().await?;

        // Apply the schedule to the latest disk state under the persistence lock.
        job_manager
            .reschedule_job(id, cron_expression.clone())
            .await?;

        // Confirm the change to the user
        println!("Updated job {} schedule to '{}'", id, cron_expression);

        Ok(())
    })
}

/// Run the daemon internal process
fn run_daemon_internal() -> Result<()> {
    // Create the runtime
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;

    // Run the async block
    rt.block_on(async {
        use crate::daemon::DaemonRunner;

        // Set up logging
        env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or("debug"), // switched default from "info" to "debug"
        )
        .init();

        log::info!("Starting daemon internal process");

        // Capture startup and runtime failures so PID cleanup runs on every exit path.
        let result = async {
            let mut daemon_runner = DaemonRunner::load().await?;
            daemon_runner.run().await
        }
        .await;
        if let Ok(data_dir) = crate::config::Config::default_data_dir() {
            Daemon::new(data_dir).remove_owned_pid();
        }
        result
    })
}

// ─── logs command ────────────────────────────────────────────────────────────

/// Run-boundary marker prefix used when writing log files.
/// Lines that match this pattern are run headers or footers injected by the
/// daemon; they are hidden unless `--timestamps` is passed.
const RUN_MARKER_PREFIX: &str = "=== Run ";

/// Return `true` when a line is a run-boundary marker (header or footer).
fn is_run_marker(line: &str) -> bool {
    line.starts_with(RUN_MARKER_PREFIX) && line.ends_with(" ===")
}

/// Apply display filters to raw log-file content.
///
/// * When `show_timestamps` is `false` the run-boundary marker lines are
///   stripped from the output.
/// * When `limit` is `Some(n)` only the last `n` lines of the (already
///   filtered) output are returned.
fn filter_log_lines(content: &str, show_timestamps: bool, limit: Option<usize>) -> String {
    // Split into lines while preserving the line structure
    let all_lines: Vec<&str> = content.lines().collect();

    // Optionally strip run-marker lines
    let filtered: Vec<&str> = if show_timestamps {
        all_lines
    } else {
        all_lines
            .into_iter()
            .filter(|l| !is_run_marker(l))
            .collect()
    };

    // Apply tail-style line limit
    let visible: &[&str] = match limit {
        Some(n) if n < filtered.len() => &filtered[filtered.len() - n..],
        _ => &filtered,
    };

    if visible.is_empty() {
        return String::new();
    }

    // Re-join lines and ensure a single trailing newline
    format!("{}\n", visible.join("\n"))
}

/// Print logs for a job, applying the requested display options.
///
/// # Arguments
///
/// * `id`         – Job ID
/// * `stderr`     – When `true`, show the stderr log; otherwise show stdout
/// * `lines`      – Limit the initial output to the last N lines (`None` = all)
/// * `follow`     – Continuously stream new log content (like `tail -f`)
/// * `timestamps` – Show run-boundary marker lines that include timestamps
fn show_logs(
    id: usize,
    stderr: bool,
    lines: Option<usize>,
    follow: bool,
    timestamps: bool,
) -> Result<()> {
    use crate::config::Config;

    // Load config to resolve log-file paths (no async needed; just reads paths)
    let config = Config::load()?;

    // Validate that the job exists so the user gets a meaningful error for an
    // unknown ID rather than a generic "no logs" message
    let rt = Runtime::new().map_err(|e| {
        CronrError::InitializationError(format!("Failed to create async runtime: {}", e))
    })?;
    rt.block_on(async {
        let job_manager = JobManager::load().await?;
        // get_job returns an error for unknown IDs
        job_manager.get_job(id).await?;
        Ok::<_, CronrError>(())
    })?;

    // Resolve the log file path based on the --stderr flag
    let log_path = if stderr {
        config.stderr_log_path(id)
    } else {
        config.stdout_log_path(id)
    };

    let stream_label = if stderr { "stderr" } else { "stdout" };

    // If the log file does not exist the job has not run yet
    if !log_path.exists() {
        println!("No {} logs found for job {}.", stream_label, id);
        if follow {
            println!("Waiting for job {} to produce {} output…", id, stream_label);
        } else {
            return Ok(());
        }
    }

    // ── Initial output ────────────────────────────────────────────────────────

    // Track the file position after the initial read so the follow loop can
    // seek directly to the first unread byte
    let mut pos: u64 = 0;

    if log_path.exists() {
        let raw = std::fs::read_to_string(&log_path).map_err(|e| {
            CronrError::ConfigError(format!("Failed to read log file: {}", e))
        })?;

        let output = filter_log_lines(&raw, timestamps, lines);
        print!("{}", output);
        let _ = io::stdout().flush();

        // Record the current end-of-file position so the follow loop resumes
        // from here without re-printing already-seen content
        pos = std::fs::metadata(&log_path)
            .map_err(|e| CronrError::ConfigError(format!("Failed to stat log file: {}", e)))?
            .len();
    }

    // ── Follow / streaming mode ───────────────────────────────────────────────

    if follow {
        println!("--- streaming {} logs for job {} (Ctrl-C to stop) ---", stream_label, id);
        let _ = io::stdout().flush();

        loop {
            std::thread::sleep(std::time::Duration::from_millis(250));

            // The file may not exist yet if the job has never run
            if !log_path.exists() {
                continue;
            }

            let new_size = match std::fs::metadata(&log_path) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            if new_size < pos {
                // Log was rotated / truncated; restart from the beginning
                pos = 0;
            }

            if new_size == pos {
                // No new bytes written since last check
                continue;
            }

            // Read only the new bytes that appeared since the last check
            let mut file = std::fs::File::open(&log_path).map_err(|e| {
                CronrError::ConfigError(format!("Failed to open log file: {}", e))
            })?;
            file.seek(SeekFrom::Start(pos)).map_err(|e| {
                CronrError::ConfigError(format!("Failed to seek log file: {}", e))
            })?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map_err(|e| {
                CronrError::ConfigError(format!("Failed to read log file: {}", e))
            })?;

            pos = new_size;

            // Convert to string, filter markers if needed, then print
            if let Ok(chunk) = String::from_utf8(buf) {
                let output = filter_log_lines(&chunk, timestamps, None);
                print!("{}", output);
                let _ = io::stdout().flush();
            }
        }
    }

    Ok(())
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a realistic multi-run log file for use in several tests.
    fn sample_log() -> String {
        [
            "=== Run started at 2024-01-15T10:00:00Z ===",
            "first run output",
            "second line",
            "",
            "=== Run ended at 2024-01-15T10:00:01Z (exit: 0) ===",
            "",
            "=== Run started at 2024-01-15T11:00:00Z ===",
            "second run output",
            "=== Run ended at 2024-01-15T11:00:02Z (exit: 1) ===",
            "",
        ]
        .join("\n")
    }

    // ── is_run_marker ──────────────────────────────────────────────────────────

    #[test]
    fn test_is_run_marker_detects_header() {
        assert!(is_run_marker("=== Run started at 2024-01-15T10:00:00Z ==="));
    }

    #[test]
    fn test_is_run_marker_detects_footer() {
        assert!(is_run_marker(
            "=== Run ended at 2024-01-15T10:00:01Z (exit: 0) ==="
        ));
    }

    #[test]
    fn test_is_run_marker_rejects_normal_lines() {
        assert!(!is_run_marker("hello world"));
        assert!(!is_run_marker("=== not a marker"));
        assert!(!is_run_marker("Run started at something ==="));
    }

    // ── filter_log_lines – timestamps off ─────────────────────────────────────

    #[test]
    fn test_filter_strips_markers_by_default() {
        let log = sample_log();
        let result = filter_log_lines(&log, false, None);

        // Marker lines must not appear in the output
        assert!(!result.contains("=== Run"));
        // Payload lines must be preserved
        assert!(result.contains("first run output"));
        assert!(result.contains("second run output"));
    }

    #[test]
    fn test_filter_applies_line_limit() {
        let log = sample_log();
        // Request only the last 1 non-marker line; blank separator lines still count
        let result = filter_log_lines(&log, false, Some(1));
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "second run output");
    }

    #[test]
    fn test_filter_limit_larger_than_content_shows_all() {
        let log = sample_log();
        let result_limited = filter_log_lines(&log, false, Some(9999));
        let result_unlimited = filter_log_lines(&log, false, None);
        assert_eq!(result_limited, result_unlimited);
    }

    // ── filter_log_lines – timestamps on ──────────────────────────────────────

    #[test]
    fn test_filter_preserves_markers_when_timestamps_on() {
        let log = sample_log();
        let result = filter_log_lines(&log, true, None);

        assert!(result.contains("=== Run started at 2024-01-15T10:00:00Z ==="));
        assert!(result.contains("=== Run ended at 2024-01-15T11:00:02Z (exit: 1) ==="));
        assert!(result.contains("first run output"));
    }

    #[test]
    fn test_filter_timestamps_with_line_limit() {
        let log = sample_log();
        // Ask for just 2 lines (markers count toward the limit when timestamps is on)
        let result = filter_log_lines(&log, true, Some(2));
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    // ── edge cases ────────────────────────────────────────────────────────────

    #[test]
    fn test_filter_empty_content_returns_empty() {
        assert_eq!(filter_log_lines("", false, None), "");
        assert_eq!(filter_log_lines("", true, None), "");
    }

    #[test]
    fn test_filter_output_ends_with_newline() {
        let log = "line one\nline two\n";
        let result = filter_log_lines(log, false, None);
        assert!(result.ends_with('\n'));
    }
}

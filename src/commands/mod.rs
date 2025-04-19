use clap::{Parser, Subcommand};
use std::process;
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

    /// Check the status of the daemon and tool
    #[clap(name = "status")]
    Status,

    /// Internal command used by the daemon process
    #[clap(name = "daemon-internal", hide = true)]
    DaemonInternal,
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
        Some(Commands::Start) => start_daemon(),
        Some(Commands::DaemonStop) => stop_daemon(),
        Some(Commands::Status) => check_daemon_status(),
        Some(Commands::DaemonInternal) => run_daemon_internal(),
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
    rt.block_on(async {
        // Create the job manager
        let job_manager = JobManager::new().await?;

        // Add the job
        let id = job_manager
            .add_job(command.clone(), cron_expression.clone())
            .await?;

        // Print the job ID
        println!("Added job {} with schedule '{}'", id, cron_expression);
        println!("Command: {}", command);

        // Return success and ensure daemon is running to execute jobs
        let data_dir = job_manager.config().data_dir().to_path_buf();
        let daemon = Daemon::new(data_dir);
        // Start daemon if not already running
        if !daemon.is_running() {
            daemon.start()?;
            println!("Started daemon for job execution");
        }

        Ok(())
    })
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

    // Run the async block
    rt.block_on(async {
        // Try to load the job manager from existing configuration
        let job_manager = JobManager::load().await?;

        // Create the daemon
        let daemon = Daemon::new(job_manager.config().data_dir().to_path_buf());

        // Check if the daemon is already running
        if daemon.is_running() {
            println!("Daemon is already running.");
            return Ok(());
        }

        // Start the daemon
        daemon.start()?;

        // Print the status
        println!("Started daemon.");

        // Return success
        Ok(())
    })
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
        // Load or initialize the job manager (initialize if data dir missing)
        let job_manager = match JobManager::load().await {
            Ok(jm) => jm,
            Err(CronrError::ConfigError(_)) => JobManager::new().await?,
            Err(e) => return Err(e),
        };

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

        // Create the daemon runner using load() instead of new() to ensure jobs persist across restarts
        let mut daemon_runner = DaemonRunner::load().await?;

        // Log that we're restoring jobs from previous configuration
        log::info!("Restoring jobs from existing configuration");

        // Run the daemon
        daemon_runner.run().await?;

        // This should never return
        process::exit(0);
    })
}

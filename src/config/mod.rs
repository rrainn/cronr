use nix::fcntl::{FlockArg, flock};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::errors::{CronrError, Result, path_error_to_config_error};
use crate::job::Job;
use crate::logger::LogRotation;

/// Configuration for the cron manager
#[derive(Debug, Clone)]
pub struct Config {
    /// The data directory
    data_dir: PathBuf,

    /// Log rotation configuration
    log_rotation: LogRotation,
}

impl Config {
    /// Create a new configuration with the default data directory
    pub fn new() -> Result<Self> {
        // Get the default data directory
        let data_dir = Self::default_data_dir()?;

        // Create the data directory (no error if it already exists)
        fs::create_dir_all(&data_dir).map_err(|e| path_error_to_config_error(&data_dir, e))?;

        // Create the log directory
        fs::create_dir_all(data_dir.join("logs"))
            .map_err(|e| path_error_to_config_error(&data_dir.join("logs"), e))?;

        // Set up log rotation with 5MB maximum size
        let log_rotation = LogRotation::new(5 * 1024 * 1024);

        Ok(Config {
            data_dir,
            log_rotation,
        })
    }

    /// Load an existing configuration from the default data directory
    pub fn load() -> Result<Self> {
        // Get the default data directory
        let data_dir = Self::default_data_dir()?;

        // Check if data directory exists and fail if it doesn't
        if !data_dir.exists() {
            return Err(CronrError::ConfigError(format!(
                "Data directory {} does not exist. Run 'cronr create' first to initialize.",
                data_dir.display()
            )));
        }

        // Set up log rotation with 5MB maximum size
        let log_rotation = LogRotation::new(5 * 1024 * 1024);

        Ok(Config {
            data_dir,
            log_rotation,
        })
    }

    /// Get the default data directory
    pub fn default_data_dir() -> Result<PathBuf> {
        // Get the home directory
        let home_dir = dirs::home_dir()
            .ok_or_else(|| CronrError::ConfigError("Could not find home directory".into()))?;

        // Return the data directory
        Ok(home_dir.join(".cronr"))
    }

    /// Create a new configuration with the given data directory
    /// This is used only in tests
    #[cfg(test)]
    pub fn with_data_dir<P: AsRef<Path>>(data_dir: P) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();

        // Create the data directory if it doesn't exist
        fs::create_dir_all(&data_dir).map_err(|e| path_error_to_config_error(&data_dir, e))?;

        // Create the log directory if it doesn't exist
        fs::create_dir_all(data_dir.join("logs"))
            .map_err(|e| path_error_to_config_error(&data_dir.join("logs"), e))?;

        // Set up log rotation with 5MB maximum size
        let log_rotation = LogRotation::new(5 * 1024 * 1024);

        Ok(Config {
            data_dir,
            log_rotation,
        })
    }

    /// Get the data directory
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Get the jobs file path
    pub fn jobs_file(&self) -> PathBuf {
        self.data_dir.join("jobs.json")
    }

    /// Get the stdout log path for a job
    pub fn stdout_log_path(&self, job_id: usize) -> PathBuf {
        self.data_dir
            .join("logs")
            .join(format!("{}.out.log", job_id))
    }

    /// Get the stderr log path for a job
    pub fn stderr_log_path(&self, job_id: usize) -> PathBuf {
        self.data_dir
            .join("logs")
            .join(format!("{}.err.log", job_id))
    }

    /// Get the log rotation configuration
    pub fn log_rotation(&self) -> &LogRotation {
        &self.log_rotation
    }

    /// Update a single job's persisted state (next_run, last_executed) in the jobs file.
    /// This is called from the job executor after each run to keep the on-disk state
    /// in sync with the in-memory state, so that daemon reload cycles and restarts
    /// see accurate schedule information.
    pub fn update_job_state(&self, job_id: usize, job: &crate::job::Job) -> Result<()> {
        self.transaction(|jobs, _| {
            // A completed executor must never resurrect a removed job or undo a CLI edit.
            if let Some(current) = jobs.get_mut(&job_id) {
                current.last_executed = job.last_executed;
                current.last_run_status = job.last_run_status.clone();
                if current.cron_expression == job.cron_expression {
                    current.next_run = job.next_run;
                }
            }
            Ok(())
        })
        .map(|_| ())
    }

    /// Lock a stable sidecar inode so replacement of jobs.json cannot bypass exclusion.
    fn transaction<T>(
        &self,
        mutation: impl FnOnce(&mut HashMap<usize, Job>, &mut usize) -> Result<T>,
    ) -> Result<(T, HashMap<usize, Job>)> {
        let lock_path = self.data_dir.join("jobs.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| path_error_to_config_error(&lock_path, e))?;
        flock(lock.as_raw_fd(), FlockArg::LockExclusive).map_err(|e| {
            CronrError::ConfigError(format!("Failed to lock {}: {}", lock_path.display(), e))
        })?;
        // Each open owns its lock; closing this file releases it even on early errors.
        let (mut jobs, mut next_id) = JobManager::load_jobs(self)?;
        let result = mutation(&mut jobs, &mut next_id)?;
        self.publish_jobs(&jobs, next_id)?;
        Ok((result, jobs))
    }

    /// Publish a fully flushed snapshot using an exclusively created file in the same directory.
    fn publish_jobs(&self, jobs: &HashMap<usize, Job>, next_id: usize) -> Result<()> {
        let jobs_file = self.jobs_file();
        let mut temporary = tempfile::NamedTempFile::new_in(&self.data_dir)
            .map_err(|e| path_error_to_config_error(&jobs_file, e))?;
        {
            let mut writer = BufWriter::new(temporary.as_file_mut());
            serde_json::to_writer_pretty(
                &mut writer,
                &serde_json::json!({"jobs": jobs, "next_id": next_id}),
            )
            .map_err(|e| CronrError::ConfigError(format!("Failed to write jobs file: {}", e)))?;
            writer
                .flush()
                .map_err(|e| path_error_to_config_error(&jobs_file, e))?;
        }
        temporary
            .as_file()
            .sync_all()
            .map_err(|e| path_error_to_config_error(&jobs_file, e))?;
        temporary
            .persist(&jobs_file)
            .map_err(|e| path_error_to_config_error(&jobs_file, e.error))?;
        Ok(())
    }
}

/// Manager for cron jobs
#[derive(Clone)]
pub struct JobManager {
    /// The configuration
    config: Config,

    /// A local read snapshot; successful mutations refresh it from the locked disk transaction.
    jobs: Arc<Mutex<HashMap<usize, Job>>>,
}

impl JobManager {
    /// Create a new job manager with the default configuration
    pub async fn new() -> Result<Self> {
        Self::with_config(Config::new()?).await
    }

    /// Load a snapshot using the caller's configured storage directory.
    pub async fn with_config(config: Config) -> Result<Self> {
        // Load the jobs
        let (jobs, _) = Self::load_jobs(&config)?;

        Ok(JobManager {
            config,
            jobs: Arc::new(Mutex::new(jobs)),
        })
    }

    /// Load an existing job manager
    pub async fn load() -> Result<Self> {
        Self::with_config(Config::load()?).await
    }

    /// Get the configuration
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Add a new job
    pub async fn add_job(&self, command: String, cron_expression: String) -> Result<usize> {
        let job = Job::new(command, cron_expression)?;
        self.mutate(move |jobs, next_id| {
            let id = *next_id;
            *next_id = next_id
                .checked_add(1)
                .ok_or_else(|| CronrError::ConfigError("Job IDs exhausted".into()))?;
            jobs.insert(id, job);
            Ok(id)
        })
        .await
    }

    /// Keep cache updates ordered while blocking file locks and disk I/O run off the async worker.
    async fn mutate<T: Send + 'static>(
        &self,
        mutation: impl FnOnce(&mut HashMap<usize, Job>, &mut usize) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let mut cache = self.jobs.clone().lock_owned().await;
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let (result, jobs) = config.transaction(mutation)?;
            *cache = jobs;
            Ok(result)
        })
        .await
        .map_err(|e| CronrError::ConfigError(format!("Job persistence task failed: {}", e)))?
    }

    /// Get a job
    pub async fn get_job(&self, id: usize) -> Result<Job> {
        // Get the jobs
        let jobs = self.jobs.lock().await;

        // Get the job
        jobs.get(&id).cloned().ok_or(CronrError::InvalidJobId(id))
    }

    /// Get all jobs
    pub async fn get_all_jobs(&self) -> HashMap<usize, Job> {
        // Get the jobs
        let jobs = self.jobs.lock().await;

        // Return a copy of the jobs
        jobs.clone()
    }

    /// Reschedule the latest persisted job, preserving concurrent completion metadata.
    pub async fn reschedule_job(&self, id: usize, cron_expression: String) -> Result<()> {
        self.mutate(move |jobs, _| {
            jobs.get_mut(&id)
                .ok_or(CronrError::InvalidJobId(id))?
                .reschedule(cron_expression)
        })
        .await
    }

    /// Remove only the requested job from the latest snapshot.
    pub async fn remove_job(&self, id: usize) -> Result<()> {
        self.mutate(move |jobs, _| {
            jobs.remove(&id).ok_or(CronrError::InvalidJobId(id))?;
            Ok(())
        })
        .await
    }

    /// Load jobs from the jobs file
    fn load_jobs(config: &Config) -> Result<(HashMap<usize, Job>, usize)> {
        // Get the jobs file path
        let jobs_file = config.jobs_file();

        // Only a missing file means fresh state; permission and other I/O failures remain errors.
        let file = match File::open(&jobs_file) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((HashMap::new(), 0));
            }
            Err(error) => return Err(path_error_to_config_error(&jobs_file, error)),
        };
        let reader = BufReader::new(file);

        // Parse JSON into a value
        let value: serde_json::Value = serde_json::from_reader(reader)
            .map_err(|e| CronrError::ConfigError(format!("Failed to parse jobs file: {}", e)))?;

        // Determine if JSON includes metadata
        let (raw_map, mut next_id) = if let Some(meta) = value.get("next_id") {
            // New format with next_id and jobs
            let id = meta
                .as_u64()
                .and_then(|id| usize::try_from(id).ok())
                .ok_or_else(|| CronrError::ConfigError("Invalid next_id in jobs file".into()))?;
            let jobs_val = value
                .get("jobs")
                .ok_or_else(|| CronrError::ConfigError("Missing jobs in jobs file".into()))?;
            let map: HashMap<String, Job> =
                serde_json::from_value(jobs_val.clone()).map_err(|e| {
                    CronrError::ConfigError(format!("Failed to parse jobs section: {}", e))
                })?;
            (map, id)
        } else {
            // Legacy format: direct mapping of ID to Job
            let map: HashMap<String, Job> = serde_json::from_value(value.clone()).map_err(|e| {
                CronrError::ConfigError(format!("Failed to parse jobs file: {}", e))
            })?;
            (map, 0)
        };

        // Convert keys to usize and collect jobs
        let mut jobs = HashMap::new();
        for (id_str, job) in raw_map {
            let id = id_str
                .parse::<usize>()
                .map_err(|_| CronrError::ConfigError(format!("Invalid job ID: {}", id_str)))?;
            jobs.insert(id, job);
            // Calculate the next ID as max(existing+1, metadata)
            let following = id
                .checked_add(1)
                .ok_or_else(|| CronrError::ConfigError("Job IDs exhausted".into()))?;
            next_id = next_id.max(following);
        }

        Ok((jobs, next_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_default_data_dir() {
        // Get the default data directory
        let data_dir = Config::default_data_dir().unwrap();

        // Check that it's in the home directory
        assert!(data_dir.to_string_lossy().contains(".cronr"));
    }

    #[test]
    fn test_log_rotation_size() {
        // Create a temporary directory
        let temp_dir = tempdir().unwrap();

        // Create a LogRotation from the Config to test default size
        let config = Config::with_data_dir(temp_dir.path()).unwrap();
        let rotation = config.log_rotation().clone();

        // Verify that the rotation size is exactly 5MB
        assert_eq!(rotation.max_size(), 5 * 1024 * 1024);
    }

    #[tokio::test]
    async fn test_job_manager() {
        // Create a temporary directory
        let temp_dir = tempdir().unwrap();
        let temp_path = temp_dir.path().to_path_buf();

        // Create a configuration
        let config = Config::with_data_dir(temp_path).unwrap();

        // Create a job manager
        let job_manager = JobManager::with_config(config).await.unwrap();

        // Add a job
        let id = job_manager
            .add_job("echo test".to_string(), "0 * * * * *".to_string())
            .await
            .unwrap();

        // Get the job
        let job = job_manager.get_job(id).await.unwrap();

        // Check the job
        assert_eq!(job.command(), "echo test");
        assert_eq!(job.cron_expression(), "0 * * * * *");

        // Remove the job
        job_manager.remove_job(id).await.unwrap();

        // Try to get the job (should fail)
        assert!(job_manager.get_job(id).await.is_err());
    }

    #[tokio::test]
    async fn test_job_id_stability() {
        // Create a temporary directory
        let temp_dir = tempdir().unwrap();
        let temp_path = temp_dir.path().to_path_buf();

        // Create a configuration
        let config = Config::with_data_dir(temp_path).unwrap();

        // Create a job manager
        let job_manager = JobManager::with_config(config).await.unwrap();

        // Add three jobs
        let id1 = job_manager
            .add_job("echo test1".to_string(), "0 * * * * *".to_string())
            .await
            .unwrap();
        let id2 = job_manager
            .add_job("echo test2".to_string(), "0 * * * * *".to_string())
            .await
            .unwrap();
        let id3 = job_manager
            .add_job("echo test3".to_string(), "0 * * * * *".to_string())
            .await
            .unwrap();

        // Remove the middle job
        job_manager.remove_job(id2).await.unwrap();

        // Add a new job and ensure it gets a new ID (not reusing id2)
        let id4 = job_manager
            .add_job("echo test4".to_string(), "0 * * * * *".to_string())
            .await
            .unwrap();

        // Verify that the new ID is not the same as the deleted one
        assert_ne!(id4, id2);

        // Verify ID ordering is maintained
        assert!(id1 < id2);
        assert!(id2 < id3);
        assert!(id3 < id4);
    }

    /// Test that update_job_state persists next_run and last_executed to disk.
    /// This ensures the daemon reload cycle sees accurate schedule data after execution.
    #[tokio::test]
    async fn test_update_job_state_persists_to_disk() {
        // Set up a temp directory with a job manager
        let temp_dir = tempdir().unwrap();
        let config = Config::with_data_dir(temp_dir.path()).unwrap();
        let job_manager = JobManager::with_config(config.clone()).await.unwrap();

        // Add a job and save it to disk
        let id = job_manager
            .add_job("echo hello".to_string(), "0 * * * * *".to_string())
            .await
            .unwrap();

        // Get the job and verify initial state
        let mut job = job_manager.get_job(id).await.unwrap();
        assert!(
            job.last_executed.is_none(),
            "last_executed should be None initially"
        );

        // Simulate execution by calling set_as_run
        job.set_as_run();
        let updated_next_run = job.next_run();
        let updated_last_executed = job.last_executed;
        assert!(
            updated_last_executed.is_some(),
            "last_executed should be set after run"
        );

        // Persist the updated state to disk
        config.update_job_state(id, &job).unwrap();

        // Reload from disk and verify the persisted state
        let reloaded_manager = JobManager::with_config(config).await.unwrap();
        let reloaded_job = reloaded_manager.get_job(id).await.unwrap();
        assert_eq!(
            reloaded_job.last_executed, updated_last_executed,
            "Reloaded last_executed should match the persisted value"
        );
        assert_eq!(
            reloaded_job.next_run(),
            updated_next_run,
            "Reloaded next_run should match the persisted value"
        );
    }
}

#[cfg(test)]
mod persistence_regressions {
    use super::*;
    use crate::job::JobRunStatus;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};

    /// A CLI manager loaded before completion must not overwrite the completed job.
    #[tokio::test]
    async fn stale_cli_mutation_preserves_completion() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(directory.path()).unwrap();
        let manager = JobManager::with_config(config.clone()).await.unwrap();
        let id = manager
            .add_job("true".into(), "0 * * * * *".into())
            .await
            .unwrap();
        let mut completed = manager.get_job(id).await.unwrap();
        completed.set_as_run();
        completed.last_run_status = Some(JobRunStatus::Success);
        config.update_job_state(id, &completed).unwrap();
        manager
            .add_job("true".into(), "0 * * * * *".into())
            .await
            .unwrap();
        let reloaded = JobManager::with_config(config).await.unwrap();
        assert_eq!(
            reloaded.get_job(id).await.unwrap().last_executed,
            completed.last_executed
        );
    }

    /// Different timestamp lengths expose both shared-inode corruption and lost updates.
    #[tokio::test]
    async fn simultaneous_completions_survive_reload() {
        for _ in 0..50 {
            let directory = tempfile::tempdir().unwrap();
            let config = Config::with_data_dir(directory.path()).unwrap();
            let manager = JobManager::with_config(config.clone()).await.unwrap();
            for _ in 0..2 {
                manager
                    .add_job("true".into(), "0 * * * * *".into())
                    .await
                    .unwrap();
            }
            let stop_reader = Arc::new(AtomicBool::new(false));
            let reader_stop = stop_reader.clone();
            let reader_config = config.clone();
            // Readers need no lock because publication replaces a complete, closed snapshot.
            let reader = std::thread::spawn(move || {
                while !reader_stop.load(Ordering::Acquire) {
                    let bytes = fs::read(reader_config.jobs_file()).unwrap();
                    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
                }
            });
            let barrier = Arc::new(Barrier::new(2));
            let mut threads = Vec::new();
            let mut expected_timestamps = Vec::new();
            for id in 0..2 {
                let mut job = manager.get_job(id).await.unwrap();
                job.last_executed = Some(
                    chrono::DateTime::parse_from_rfc3339(if id == 0 {
                        "2025-01-01T00:00:00.123456789Z"
                    } else {
                        "2025-01-01T00:00:00.123456Z"
                    })
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                );
                job.last_run_status = Some(JobRunStatus::Success);
                expected_timestamps.push(job.last_executed);
                let config = config.clone();
                let barrier = barrier.clone();
                threads.push(std::thread::spawn(move || {
                    barrier.wait();
                    config.update_job_state(id, &job)
                }));
            }
            let outcomes: Vec<_> = threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect();
            stop_reader.store(true, Ordering::Release);
            reader.join().unwrap();
            for outcome in outcomes {
                outcome.unwrap();
            }
            let reloaded = JobManager::with_config(config).await.unwrap();
            for (id, expected_timestamp) in expected_timestamps.iter().enumerate() {
                let job = reloaded.get_job(id).await.unwrap();
                assert_eq!(job.last_run_status, Some(JobRunStatus::Success));
                assert_eq!(job.last_executed, *expected_timestamp);
            }
        }
    }
}

#[cfg(test)]
mod transaction_regressions {
    use super::*;
    use crate::job::JobRunStatus;

    /// Both transaction orders must retain an edited schedule and execution results.
    #[tokio::test]
    async fn completion_and_edit_merge_in_both_orders() {
        for completion_first in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let config = Config::with_data_dir(directory.path()).unwrap();
            let manager = JobManager::with_config(config.clone()).await.unwrap();
            let id = manager
                .add_job("true".into(), "0 * * * * *".into())
                .await
                .unwrap();
            let mut completed = manager.get_job(id).await.unwrap();
            completed.set_as_run();
            completed.last_run_status = Some(JobRunStatus::Success);
            if completion_first {
                config.update_job_state(id, &completed).unwrap();
            }
            manager
                .reschedule_job(id, "0 0 * * * *".into())
                .await
                .unwrap();
            let edited_next_run = manager.get_job(id).await.unwrap().next_run;
            if !completion_first {
                config.update_job_state(id, &completed).unwrap();
            }
            let reloaded = JobManager::with_config(config.clone()).await.unwrap();
            let job = reloaded.get_job(id).await.unwrap();
            assert_eq!(job.cron_expression, "0 0 * * * *");
            assert_eq!(job.next_run, edited_next_run);
            assert_eq!(job.last_executed, completed.last_executed);
            assert_eq!(job.last_run_status, completed.last_run_status);
            manager.remove_job(id).await.unwrap();
            config.update_job_state(id, &completed).unwrap();
            assert!(
                JobManager::with_config(config)
                    .await
                    .unwrap()
                    .get_all_jobs()
                    .await
                    .is_empty()
            );
        }
    }

    /// A failed mutation leaves both the persisted snapshot and manager cache intact.
    #[tokio::test]
    async fn corrupt_state_is_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(directory.path()).unwrap();
        let manager = JobManager::with_config(config.clone()).await.unwrap();
        let id = manager
            .add_job("true".into(), "0 * * * * *".into())
            .await
            .unwrap();
        let original = manager.get_job(id).await.unwrap();
        let corrupt = b"{}1\n}";
        fs::write(config.jobs_file(), corrupt).unwrap();
        let error = manager.remove_job(id).await.unwrap_err();
        assert!(error.to_string().contains("trailing characters"));
        assert!(
            config
                .update_job_state(id, &original)
                .unwrap_err()
                .to_string()
                .contains("trailing characters")
        );
        assert_eq!(fs::read(config.jobs_file()).unwrap(), corrupt);
        assert!(manager.get_job(id).await.is_ok());
    }

    /// Legacy maps retain their execution state and allocate IDs above all existing jobs.
    #[tokio::test]
    async fn legacy_map_migrates_without_losing_state() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(directory.path()).unwrap();
        let mut job = Job::new("true".into(), "0 * * * * *".into()).unwrap();
        job.set_as_run();
        job.last_run_status = Some(JobRunStatus::Success);
        fs::write(
            config.jobs_file(),
            serde_json::to_vec(&serde_json::json!({"7": job})).unwrap(),
        )
        .unwrap();
        let manager = JobManager::with_config(config.clone()).await.unwrap();
        let id = manager
            .add_job("true".into(), "0 * * * * *".into())
            .await
            .unwrap();
        assert_eq!(id, 8);
        let reloaded = JobManager::with_config(config).await.unwrap();
        assert_eq!(
            reloaded.get_job(7).await.unwrap().last_executed,
            job.last_executed
        );
        assert_eq!(
            reloaded.get_job(7).await.unwrap().last_run_status,
            job.last_run_status
        );
    }

    /// Child processes use the real persistence boundary and an isolated directory.
    #[test]
    fn persistence_child() {
        let Ok(directory) = std::env::var("CRONR_PERSISTENCE_TEST_DIR") else {
            return;
        };
        let config = Config::with_data_dir(directory).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let manager = JobManager::with_config(config.clone()).await.unwrap();
            let id: usize = std::env::var("CRONR_PERSISTENCE_TEST_ID")
                .unwrap()
                .parse()
                .unwrap();
            let mut job = manager.get_job(id).await.unwrap();
            job.set_as_run();
            job.last_run_status = Some(JobRunStatus::Success);
            config.update_job_state(id, &job).unwrap();
            manager
                .add_job(format!("echo child {id}"), "0 * * * * *".into())
                .await
                .unwrap();
        });
    }

    /// Independent processes must coordinate on the same sidecar, including ID allocation.
    #[tokio::test]
    async fn separate_processes_preserve_updates() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(directory.path()).unwrap();
        let manager = JobManager::with_config(config.clone()).await.unwrap();
        for _ in 0..8 {
            manager
                .add_job("true".into(), "0 * * * * *".into())
                .await
                .unwrap();
        }
        let mut children = Vec::new();
        for id in 0..8 {
            children.push(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "config::transaction_regressions::persistence_child",
                    ])
                    .env("CRONR_PERSISTENCE_TEST_DIR", directory.path())
                    .env("CRONR_PERSISTENCE_TEST_ID", id.to_string())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        let reloaded = JobManager::with_config(config).await.unwrap();
        assert_eq!(reloaded.get_all_jobs().await.len(), 16);
        for id in 0..8 {
            assert_eq!(
                reloaded.get_job(id).await.unwrap().last_run_status,
                Some(JobRunStatus::Success)
            );
        }
    }
}

use dirs;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
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
        let jobs_file = self.jobs_file();

        // Nothing to update if the jobs file doesn't exist yet
        if !jobs_file.exists() {
            return Ok(());
        }

        // Read the current jobs file
        let file =
            File::open(&jobs_file).map_err(|e| path_error_to_config_error(&jobs_file, e))?;
        let reader = BufReader::new(file);
        let mut value: serde_json::Value = serde_json::from_reader(reader)
            .map_err(|e| CronrError::ConfigError(format!("Failed to parse jobs file: {}", e)))?;

        // Navigate to the correct job entry (supports both new and legacy format)
        let id_str = job_id.to_string();
        let jobs_obj = if let Some(jobs) = value.get_mut("jobs") {
            jobs
        } else {
            &mut value
        };

        // Serialize the updated job and replace the entry
        if let Some(job_value) = jobs_obj.get_mut(&id_str) {
            let job_json = serde_json::to_value(job)
                .map_err(|e| CronrError::ConfigError(format!("Failed to serialize job: {}", e)))?;
            *job_value = job_json;
        }

        // Write back atomically via a temp file + rename
        let temp_file = jobs_file.with_file_name(format!(
            "{}.tmp",
            jobs_file.file_name().unwrap().to_string_lossy()
        ));
        let file =
            File::create(&temp_file).map_err(|e| path_error_to_config_error(&temp_file, e))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, &value)
            .map_err(|e| CronrError::ConfigError(format!("Failed to write jobs file: {}", e)))?;
        writer
            .flush()
            .map_err(|e| CronrError::ConfigError(format!("Failed to flush jobs file: {}", e)))?;
        fs::rename(&temp_file, &jobs_file)
            .map_err(|e| path_error_to_config_error(&jobs_file, e))?;

        Ok(())
    }
}

/// Manager for cron jobs
#[derive(Clone)]
pub struct JobManager {
    /// The configuration
    config: Config,

    /// The jobs
    jobs: Arc<Mutex<HashMap<usize, Job>>>,

    /// The next job ID
    next_id: Arc<Mutex<usize>>,
}

impl JobManager {
    /// Create a new job manager with the default configuration
    pub async fn new() -> Result<Self> {
        // Create the configuration
        let config = Config::new()?;

        // Load the jobs
        let (jobs, next_id) = Self::load_jobs(&config).await?;

        Ok(JobManager {
            config,
            jobs: Arc::new(Mutex::new(jobs)),
            next_id: Arc::new(Mutex::new(next_id)),
        })
    }

    /// Create a new job manager with the given configuration
    /// This is used only in tests
    #[cfg(test)]
    pub async fn with_config(config: Config) -> Result<Self> {
        // Load the jobs
        let (jobs, next_id) = Self::load_jobs(&config).await?;

        Ok(JobManager {
            config,
            jobs: Arc::new(Mutex::new(jobs)),
            next_id: Arc::new(Mutex::new(next_id)),
        })
    }

    /// Load an existing job manager
    pub async fn load() -> Result<Self> {
        // Load the configuration
        let config = Config::load()?;

        // Load the jobs
        let (jobs, next_id) = Self::load_jobs(&config).await?;

        Ok(JobManager {
            config,
            jobs: Arc::new(Mutex::new(jobs)),
            next_id: Arc::new(Mutex::new(next_id)),
        })
    }

    /// Get the configuration
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Add a new job
    pub async fn add_job(&self, command: String, cron_expression: String) -> Result<usize> {
        // Create the job
        let job = Job::new(command, cron_expression)?;

        // Get the next ID
        let id = {
            let mut next_id = self.next_id.lock().await;
            let id = *next_id;
            *next_id += 1;
            id
        };

        // Add the job
        {
            let mut jobs = self.jobs.lock().await;
            jobs.insert(id, job);
        }

        // Save the jobs
        self.save_jobs().await?;

        Ok(id)
    }

    /// Get a job
    pub async fn get_job(&self, id: usize) -> Result<Job> {
        // Get the jobs
        let jobs = self.jobs.lock().await;

        // Get the job
        jobs.get(&id)
            .cloned()
            .ok_or_else(|| CronrError::InvalidJobId(id))
    }

    /// Get all jobs
    pub async fn get_all_jobs(&self) -> HashMap<usize, Job> {
        // Get the jobs
        let jobs = self.jobs.lock().await;

        // Return a copy of the jobs
        jobs.clone()
    }

    /// Update a job
    // pub async fn update_job(&self, id: usize, job: Job) -> Result<()> {
    // 	// Get the jobs
    // 	let mut jobs = self.jobs.lock().await;

    // 	// Check if the job exists
    // 	if !jobs.contains_key(&id) {
    // 		return Err(CronrError::InvalidJobId(id));
    // 	}

    // 	// Update the job
    // 	jobs.insert(id, job);

    // 	// Save the jobs
    // 	drop(jobs);
    // 	self.save_jobs().await?;

    // 	Ok(())
    // }

    /// Remove a job
    pub async fn remove_job(&self, id: usize) -> Result<()> {
        // Get the jobs
        let mut jobs = self.jobs.lock().await;

        // Check if the job exists
        if !jobs.contains_key(&id) {
            return Err(CronrError::InvalidJobId(id));
        }

        // Remove the job
        jobs.remove(&id);

        // Save the jobs
        drop(jobs);
        self.save_jobs().await?;

        Ok(())
    }

    /// Load jobs from the jobs file
    async fn load_jobs(config: &Config) -> Result<(HashMap<usize, Job>, usize)> {
        // Get the jobs file path
        let jobs_file = config.jobs_file();

        // If file doesn't exist, start fresh with no jobs and next ID 0
        if !jobs_file.exists() {
            return Ok((HashMap::new(), 0));
        }

        // Open and read the file
        let file = File::open(&jobs_file).map_err(|e| path_error_to_config_error(&jobs_file, e))?;
        let reader = BufReader::new(file);

        // Parse JSON into a value
        let value: serde_json::Value = serde_json::from_reader(reader)
            .map_err(|e| CronrError::ConfigError(format!("Failed to parse jobs file: {}", e)))?;

        // Determine if JSON includes metadata
        let (raw_map, mut next_id) = if let Some(meta) = value.get("next_id") {
            // New format with next_id and jobs
            let id = meta
                .as_u64()
                .ok_or_else(|| CronrError::ConfigError("Invalid next_id in jobs file".into()))?
                as usize;
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
            if id + 1 > next_id {
                next_id = id + 1;
            }
        }

        Ok((jobs, next_id))
    }

    /// Save jobs to the jobs file
    async fn save_jobs(&self) -> Result<()> {
        // Get the jobs file path
        let jobs_file = self.config.jobs_file();

        // Create a temporary file
        let temp_file = jobs_file.with_file_name(format!(
            "{}.tmp",
            jobs_file.file_name().unwrap().to_string_lossy()
        ));

        // Create the writer
        let file =
            File::create(&temp_file).map_err(|e| path_error_to_config_error(&temp_file, e))?;
        let mut writer = BufWriter::new(file);

        // Clone jobs into a local owned map and get next_id
        let jobs_map: HashMap<String, Job> = {
            let jobs_guard = self.jobs.lock().await;
            jobs_guard
                .iter()
                .map(|(id, job)| (id.to_string(), job.clone()))
                .collect()
        };
        let next_id = { *self.next_id.lock().await };

        // Build wrapper with metadata and jobs
        let wrapper = serde_json::json!({
            "next_id": next_id,
            "jobs": jobs_map
        });

        // Write the JSON
        serde_json::to_writer_pretty(&mut writer, &wrapper)
            .map_err(|e| CronrError::ConfigError(format!("Failed to write jobs file: {}", e)))?;
        writer
            .flush()
            .map_err(|e| CronrError::ConfigError(format!("Failed to flush jobs file: {}", e)))?;

        // Rename the temporary file to the jobs file
        fs::rename(&temp_file, &jobs_file)
            .map_err(|e| path_error_to_config_error(&jobs_file, e))?;

        Ok(())
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
        assert!(job.last_executed.is_none(), "last_executed should be None initially");

        // Simulate execution by calling set_as_run
        job.set_as_run();
        let updated_next_run = job.next_run();
        let updated_last_executed = job.last_executed;
        assert!(updated_last_executed.is_some(), "last_executed should be set after run");

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
            reloaded_job.next_run(), updated_next_run,
            "Reloaded next_run should match the persisted value"
        );
    }
}

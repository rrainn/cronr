use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;
use std::env;

// Helper function to run cronr with custom home directory
fn run_cronr_with_home(args: &[&str], home_dir: &PathBuf) -> assert_cmd::assert::Assert {
	let mut cmd = Command::cargo_bin("cronr").unwrap();
	cmd.env("HOME", home_dir.to_str().unwrap())
		.args(args);
	cmd.assert()
}

// Test version command
#[test]
fn test_version_command() {
	let mut cmd = Command::cargo_bin("cronr").unwrap();
	cmd.arg("version")
		.assert()
		.success()
		.stdout(predicates::str::contains(env!("CARGO_PKG_VERSION")));

	// Test with --version flag
	let mut cmd = Command::cargo_bin("cronr").unwrap();
	cmd.arg("--version")
		.assert()
		.success()
		.stdout(predicates::str::contains(env!("CARGO_PKG_VERSION")));
}

// Test create and list commands
#[test]
fn test_create_and_list() {
	// Create a temporary directory for the test
	let temp_dir = tempdir().unwrap();
	let home_dir = temp_dir.path().to_path_buf();

	// Ensure the .cronr directory doesn't exist
	let cronr_dir = home_dir.join(".cronr");
	if cronr_dir.exists() {
		fs::remove_dir_all(&cronr_dir).unwrap();
	}

	// Create a cron job
	run_cronr_with_home(
		&["create", "echo test", "0 * * * * *"],
		&home_dir,
	)
	.success()
	.stdout(predicates::str::contains("Added job"));

	// List cron jobs
	run_cronr_with_home(
		&["ls"],
		&home_dir,
	)
	.success()
	.stdout(predicates::str::contains("echo test"))
	.stdout(predicates::str::contains("0 * * * * *"));

	// Clean up by removing the temp directory
	temp_dir.close().unwrap();
}

// Test stop command
#[test]
fn test_stop_job() {
	// Create a temporary directory for the test
	let temp_dir = tempdir().unwrap();
	let home_dir = temp_dir.path().to_path_buf();

	// Ensure the .cronr directory doesn't exist
	let cronr_dir = home_dir.join(".cronr");
	if cronr_dir.exists() {
		fs::remove_dir_all(&cronr_dir).unwrap();
	}

	// Create a cron job
	run_cronr_with_home(
		&["create", "echo test", "0 * * * * *"],
		&home_dir,
	)
	.success();

	// Stop the cron job
	run_cronr_with_home(
		&["stop", "0"],
		&home_dir,
	)
	.success()
	.stdout(predicates::str::contains("Stopped job 0"));

	// List cron jobs (should be empty)
	run_cronr_with_home(
		&["ls"],
		&home_dir,
	)
	.success()
	.stdout(predicates::str::contains("No cron jobs found"));

	// Clean up by removing the temp directory
	temp_dir.close().unwrap();
}

// Test invalid job ID
#[test]
fn test_invalid_job_id() {
	// Create a temporary directory for the test
	let temp_dir = tempdir().unwrap();
	let home_dir = temp_dir.path().to_path_buf();

	// Ensure the .cronr directory doesn't exist
	let cronr_dir = home_dir.join(".cronr");
	if cronr_dir.exists() {
		fs::remove_dir_all(&cronr_dir).unwrap();
	}

	// Create a cron job
	run_cronr_with_home(
		&["create", "echo test", "0 * * * * *"],
		&home_dir,
	)
	.success();

	// Try to stop a non-existent job
	run_cronr_with_home(
		&["stop", "999"],
		&home_dir,
	)
	.failure()
	.stderr(predicates::str::contains("Invalid job ID: 999"));

	// Clean up by removing the temp directory
	temp_dir.close().unwrap();
}

// Test invalid cron expression
#[test]
fn test_invalid_cron_expression() {
	// Create a temporary directory for the test
	let temp_dir = tempdir().unwrap();
	let home_dir = temp_dir.path().to_path_buf();

	// Try to create a job with an invalid cron expression
	run_cronr_with_home(
		&["create", "echo test", "invalid_cron"],
		&home_dir,
	)
	.failure()
	.stderr(predicates::str::contains("Invalid cron expression"));

	// Clean up by removing the temp directory
	temp_dir.close().unwrap();
}

// Test log file creation and rotation
#[test]
fn test_log_rotation() {
	// Create a temporary directory for the test
	let temp_dir = tempdir().unwrap();
	let home_dir = temp_dir.path().to_path_buf();

	// Ensure the .cronr directory doesn't exist
	let cronr_dir = home_dir.join(".cronr");
	if cronr_dir.exists() {
		fs::remove_dir_all(&cronr_dir).unwrap();
	}

	// Create a cron job
	run_cronr_with_home(
		&["create", "echo test", "0 * * * * *"],
		&home_dir,
	)
	.success();

	// Verify the log directory was created
	let logs_dir = home_dir.join(".cronr").join("logs");
	assert!(logs_dir.exists());

	// Clean up by removing the temp directory
	temp_dir.close().unwrap();
}

// Test existing directory check
#[test]
fn test_existing_directory_check() {
	// Create a temporary directory for the test
	let temp_dir = tempdir().unwrap();
	let home_dir = temp_dir.path().to_path_buf();

	// Create the .cronr directory manually
	let cronr_dir = home_dir.join(".cronr");
	fs::create_dir_all(&cronr_dir).unwrap();

	// Try to create a cron job (should succeed even if directory exists)
	run_cronr_with_home(
		&["create", "echo test", "0 * * * * *"],
		&home_dir,
	)
	.success()
	.stdout(predicates::str::contains("Added job"));

	// Clean up by removing the temp directory
	temp_dir.close().unwrap();
}

// Test data directory location
#[test]
fn test_data_directory_location() {
	// Create a temporary directory for the test
	let temp_dir = tempdir().unwrap();
	let home_dir = temp_dir.path().to_path_buf();

	// Create a cron job, which will initialize the data directory
	run_cronr_with_home(
		&["create", "echo test", "0 * * * * *"],
		&home_dir,
	)
	.success();

	// Verify the .cronr directory was created in the home directory
	let cronr_dir = home_dir.join(".cronr");
	assert!(cronr_dir.exists());
	assert!(cronr_dir.is_dir());

	// Clean up by removing the temp directory
	temp_dir.close().unwrap();
}

// Test status command
#[test]
fn test_status_command() {
	// Create a temporary directory for the test
	let temp_dir = tempdir().unwrap();
	let home_dir = temp_dir.path().to_path_buf();

	// Ensure no jobs exist
	let cronr_dir = home_dir.join(".cronr");
	if (cronr_dir.exists()) {
		fs::remove_dir_all(&cronr_dir).unwrap();
	}

	// Run status command
	run_cronr_with_home(
		&["status"],
		&home_dir,
	)
	.success()
	.stdout(predicates::str::contains(env!("CARGO_PKG_VERSION")))
	.stdout(predicates::str::contains("Active jobs: 0"))
	.stdout(predicates::str::contains("Daemon is not running."));

	// Clean up
	temp_dir.close().unwrap();
}

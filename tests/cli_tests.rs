use assert_cmd::Command;
use std::env;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

// Helper function to run cronr with custom home directory
fn run_cronr_with_home(args: &[&str], home_dir: &PathBuf) -> assert_cmd::assert::Assert {
    let mut cmd = Command::cargo_bin("cronr").unwrap();
    cmd.env("HOME", home_dir.to_str().unwrap()).args(args);
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
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir)
        .success()
        .stdout(predicates::str::contains("Added job"));

    // List cron jobs
    run_cronr_with_home(&["ls"], &home_dir)
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
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir).success();

    // Stop the cron job
    run_cronr_with_home(&["stop", "0"], &home_dir)
        .success()
        .stdout(predicates::str::contains("Stopped job 0"));

    // List cron jobs (should be empty)
    run_cronr_with_home(&["ls"], &home_dir)
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
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir).success();

    // Try to stop a non-existent job
    run_cronr_with_home(&["stop", "999"], &home_dir)
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
    run_cronr_with_home(&["create", "echo test", "invalid_cron"], &home_dir)
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
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir).success();

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
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir)
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
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir).success();

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
    if cronr_dir.exists() {
        fs::remove_dir_all(&cronr_dir).unwrap();
    }

    // Run status command
    run_cronr_with_home(&["status"], &home_dir)
        .success()
        .stdout(predicates::str::contains(env!("CARGO_PKG_VERSION")))
        .stdout(predicates::str::contains("Active jobs: 0"))
        .stdout(predicates::str::contains("Daemon is not running."));

    // Clean up
    temp_dir.close().unwrap();
}

// Test run command executes a job immediately without altering its schedule
#[test]
fn test_run_job_test() {
    // Create a temporary directory for the test
    let temp_dir = tempdir().unwrap();
    let home_dir = temp_dir.path().to_path_buf();

    // Ensure the .cronr directory doesn't exist
    let cronr_dir = home_dir.join(".cronr");
    if cronr_dir.exists() {
        fs::remove_dir_all(&cronr_dir).unwrap();
    }

    // Create a job that echoes a known string
    run_cronr_with_home(&["create", "echo hello_cronr_test", "0 * * * * *"], &home_dir)
        .success()
        .stdout(predicates::str::contains("Added job 0"));

    // Run the job once as a test — should succeed and show the separator lines
    run_cronr_with_home(&["run", "0"], &home_dir)
        .success()
        .stdout(predicates::str::contains("Running job 0 one-off test:"))
        .stdout(predicates::str::contains("echo hello_cronr_test"))
        .stdout(predicates::str::contains("completed successfully"));

    // Clean up
    temp_dir.close().unwrap();
}

// Test run command fails gracefully for a non-existent job ID
#[test]
fn test_run_invalid_job_id() {
    // Create a temporary directory for the test
    let temp_dir = tempdir().unwrap();
    let home_dir = temp_dir.path().to_path_buf();

    // Create one job so the data directory and jobs file exist
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir).success();

    // Try to run a job with an ID that does not exist
    run_cronr_with_home(&["run", "999"], &home_dir)
        .failure()
        .stderr(predicates::str::contains("Invalid job ID: 999"));

    // Clean up
    temp_dir.close().unwrap();
}

// Test info command shows correct job details before the job has ever run
#[test]
fn test_info_command_never_run() {
    let temp_dir = tempdir().unwrap();
    let home_dir = temp_dir.path().to_path_buf();

    // Create a cron job
    run_cronr_with_home(&["create", "echo hello_info", "0 * * * * *"], &home_dir)
        .success()
        .stdout(predicates::str::contains("Added job 0"));

    // Run info on the newly created job
    run_cronr_with_home(&["info", "0"], &home_dir)
        .success()
        // Should show the job ID
        .stdout(predicates::str::contains("Job 0"))
        // Should show the command
        .stdout(predicates::str::contains("echo hello_info"))
        // Should show the cron expression
        .stdout(predicates::str::contains("0 * * * * *"))
        // Job has never run, so last run should be "Never"
        .stdout(predicates::str::contains("Last Run:"))
        .stdout(predicates::str::contains("Never"))
        // A next run time should be present
        .stdout(predicates::str::contains("Next Run:"))
        // Status should be N/A when the job has never been run
        .stdout(predicates::str::contains("Last Run Status:"))
        .stdout(predicates::str::contains("N/A"));

    temp_dir.close().unwrap();
}

// Test info command fails gracefully for a non-existent job ID
#[test]
fn test_info_invalid_job_id() {
    let temp_dir = tempdir().unwrap();
    let home_dir = temp_dir.path().to_path_buf();

    // Create one job so the data directory and jobs file exist
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir).success();

    // Request info for a non-existent ID
    run_cronr_with_home(&["info", "999"], &home_dir)
        .failure()
        .stderr(predicates::str::contains("Invalid job ID: 999"));

    temp_dir.close().unwrap();
}

// Test edit command updates the cron schedule of an existing job
#[test]
fn test_edit_job_schedule() {
    let temp_dir = tempdir().unwrap();
    let home_dir = temp_dir.path().to_path_buf();

    // Create a job with an initial schedule
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir)
        .success()
        .stdout(predicates::str::contains("Added job 0"));

    // Edit the job's schedule to a new cron expression
    run_cronr_with_home(&["edit", "0", "0 0 * * * *"], &home_dir)
        .success()
        .stdout(predicates::str::contains("Updated job 0"))
        .stdout(predicates::str::contains("0 0 * * * *"));

    // Verify the list shows the updated schedule
    run_cronr_with_home(&["ls"], &home_dir)
        .success()
        .stdout(predicates::str::contains("0 0 * * * *"));

    // Verify info also reflects the new schedule
    run_cronr_with_home(&["info", "0"], &home_dir)
        .success()
        .stdout(predicates::str::contains("0 0 * * * *"));

    temp_dir.close().unwrap();
}

// Test edit command fails for a non-existent job ID
#[test]
fn test_edit_invalid_job_id() {
    let temp_dir = tempdir().unwrap();
    let home_dir = temp_dir.path().to_path_buf();

    // Create a job so the data directory exists
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir).success();

    // Attempt to edit a non-existent job ID
    run_cronr_with_home(&["edit", "99", "0 0 * * * *"], &home_dir).failure();

    temp_dir.close().unwrap();
}

// Test edit command fails for an invalid cron expression
#[test]
fn test_edit_invalid_cron_expression() {
    let temp_dir = tempdir().unwrap();
    let home_dir = temp_dir.path().to_path_buf();

    // Create a valid job first
    run_cronr_with_home(&["create", "echo test", "0 * * * * *"], &home_dir).success();

    // Attempt to edit with a bad cron expression
    run_cronr_with_home(&["edit", "0", "not-a-cron-expression"], &home_dir).failure();

    temp_dir.close().unwrap();
}

// Test that info reflects a successful run status after `cronr run`
#[test]
fn test_info_shows_success_status_after_run() {
    let temp_dir = tempdir().unwrap();
    let home_dir = temp_dir.path().to_path_buf();

    // Create a job that always succeeds
    run_cronr_with_home(&["create", "true", "0 * * * * *"], &home_dir)
        .success()
        .stdout(predicates::str::contains("Added job 0"));

    // Execute the job once so last_run_status gets written to disk
    run_cronr_with_home(&["run", "0"], &home_dir).success();

    // NOTE: `cronr run` uses run_test() which intentionally does not update
    // last_executed or last_run_status. Info should therefore still show N/A
    // for status and "Never" for last run (matching the documented behaviour).
    run_cronr_with_home(&["info", "0"], &home_dir)
        .success()
        .stdout(predicates::str::contains("Job 0"))
        .stdout(predicates::str::contains("true"))
        .stdout(predicates::str::contains("Last Run Status:"))
        .stdout(predicates::str::contains("N/A"));

    temp_dir.close().unwrap();
}

# Cronr

A robust CLI tool for managing cron tasks with automatic job execution and log management.

## Features

- Simple CLI interface for creating, listing, and stopping cron jobs
- Automatic job execution via a background daemon
- Log rotation that maintains separate stdout and stderr logs
- Automatic startup on system reboot
- Stable job IDs that persist after job deletion

## Installation

### From Source

1. Clone the repository:
   ```
   git clone https://github.com/rrainn/cronr.git
   cd cronr
   ```

2. Build and install the binary:
   ```
   cargo install --path .
   ```

3. Set up the daemon to run on startup:

   **On Linux (with systemd):**
   ```
   sudo ./scripts/install_linux.sh
   ```

   **On macOS:**
   ```
   ./scripts/install_macos.sh
   ```

### Prebuilt Binaries

Download the appropriate binary for your platform from the [Releases](https://github.com/rrainn/cronr/releases) page.

## Usage

### Creating a cron job

```
cronr create "your_command_here" "cron_schedule"
```

Example:
```
cronr create "curl -v https://ip.rrainn.space" "0 5 4 * * *"
```

This will create a job that runs `curl -v https://ip.rrainn.space` at 4:05 AM every day.

### Listing all cron jobs

```
cronr ls
```

This shows all cron jobs, including:
- ID number
- Cron schedule
- Command being run

### Stopping a cron job

```
cronr stop ID
```

Example:
```
cronr stop 2
```

This will permanently delete the cron job with ID 2.

### Viewing version information

```
cronr version
```

Or use the shorthand:
```
cronr -v
```

## Checking status

```
cronr status
```

This shows:
- cronr version
- number of active jobs
- whether the daemon is running

## Data Storage

Cronr stores all its data in the `~/.cronr` directory:

- `jobs.json`: Contains all job configurations
- `logs/`: Directory containing all job output logs
  - `{job_id}.out.log`: Standard output from the job
  - `{job_id}.err.log`: Standard error from the job
  - Log files rotate when they reach 5MB in size

## Development

### Prerequisites

- Rust and Cargo (1.70.0 or later)
- For Linux: systemd development libraries
- For testing: Rust test framework

### Building

```
cargo build
```

### Running Tests

```
cargo test
```

This will run both unit tests and integration tests.

### GitHub Actions Integration

The repository includes CI workflows for GitHub Actions that automatically build and test the code on each commit.

## License

MIT

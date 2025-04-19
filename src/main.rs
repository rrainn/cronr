use clap::Parser;
use std::process;

mod commands;
mod config;
mod daemon;
mod errors;
mod job;
mod logger;

use commands::{Cli, run};

fn main() {
	// Parse command-line arguments
	let cli: Cli = Cli::parse();

	// Run the command-line interface
	if let Err(err) = run(cli) {
		eprintln!("Error: {}", err);
		process::exit(1);
	}
}

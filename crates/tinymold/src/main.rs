//! The `tinymold` command-line entry point.
//!
//! Everything testable lives in the library; this file only parses arguments,
//! runs the command, and turns the outcome into a process exit code.

use std::io;
use std::process::ExitCode;

use clap::Parser;
use tinymold::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match tinymold::run(&cli, &mut io::stdout().lock()) {
        Ok(outcome) => outcome.into(),
        Err(error) => {
            eprintln!("tinymold: {error}");
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

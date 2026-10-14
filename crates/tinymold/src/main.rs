//! The `tinymold` command-line entry point.
//!
//! Everything testable lives in the library; this file only tells a
//! `RUSTC_WRAPPER` call from a command line, runs it, and turns the outcome
//! into a process exit code.

use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

use clap::Parser;
use tinymold::Cli;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let Some((program, rest)) = tinymold::wrapper_call(&args) {
        // `RUSTC_WRAPPER=tinymold`: serve the compile, or run it uncached when
        // no store can be found.
        let code = match tinymold::wrapper_config(|name| std::env::var_os(name)) {
            Some(config) => tinymold_wrap::run(&config, program, rest),
            None => tinymold_wrap::run_uncached(program, rest),
        };
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
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

use std::process::ExitCode;

mod args;
mod run;

use args::Command;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    match args::parse(argv) {
        Ok(Command::Help) => {
            print!("{}", args::usage());
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("wrec-cli {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Command::List(list_args)) => run::list(list_args),
        Ok(Command::Record(record_args)) => run::record(record_args),
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

use std::process::ExitCode;

mod args;
mod run;
mod update;

use args::Command;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    match args::parse(argv) {
        Ok(Command::Help) => {
            print!("{}", args::usage());
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!(
                "{} {}",
                wrec_channel::Channel::current().cli_name(),
                std::env::var("WREC_ARTIFACT_VERSION")
                    .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
            );
            ExitCode::SUCCESS
        }
        Ok(Command::List(list_args)) => run::list(list_args),
        Ok(Command::Record(record_args)) => run::record(record_args),
        Ok(Command::Daemon(command)) => run::daemon(command),
        Ok(Command::Jobs(args)) => run::jobs(args),
        Ok(Command::Job(command)) => run::job(command),
        Ok(Command::Update(args)) => update::update(args),
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

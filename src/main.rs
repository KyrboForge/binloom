mod download;
mod init;
mod list;
mod lockfile;
mod manifest;
mod platform;
mod sources;
mod update;

use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(author, version, about, long_about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init,
    Install,
    Update { tool: Option<String> },
    Exec,
    List,
    Path,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match &cli.command {
        Command::Init => init::init(),
        Command::List => list::list(),
        Command::Update { tool } => update::update(tool.as_deref()),
        Command::Install => {
            println!("Installing...");
            Ok(())
        }
        Command::Exec => {
            println!("Executing...");
            Ok(())
        }
        Command::Path => {
            println!("Showing path...");
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

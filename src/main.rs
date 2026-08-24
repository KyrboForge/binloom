mod init;

use std::process::ExitCode;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(author, version, about, long_about)]
struct CLI {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init,
    Install,
    Update,
    Exec,
    List,
    Path,
}

fn main() -> ExitCode {
    let cli = CLI::parse();

    let result = match &cli.command {
        Command::Init => init::init(),
        Command::Install => Ok({
            println!("Installing...");
        }),
        Command::Update => Ok({
            println!("Updating...");
        }),
        Command::Exec => Ok({
            println!("Executing...");
        }),
        Command::List => Ok({
            println!("Listing...");
        }),
        Command::Path => Ok({
            println!("Showing path...");
        }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

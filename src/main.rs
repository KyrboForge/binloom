mod init;

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
    Update,
    Exec,
    List,
    Path,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match &cli.command {
        Command::Init => init::init(),
        Command::Install => {
            println!("Installing...");
            Ok(())
        }
        Command::Update => {
            println!("Updating...");
            Ok(())
        }
        Command::Exec => {
            println!("Executing...");
            Ok(())
        }
        Command::List => {
            println!("Listing...");
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

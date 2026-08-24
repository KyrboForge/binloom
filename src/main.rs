mod download;
mod init;
mod install;
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
    #[command(about = "Initialize the repository")]
    Init,
    #[command(about = "Install the tools")]
    Install,
    #[command(about = "Update the tools")]
    Update {
        /// Tool to update; omit to update all tools
        tool: Option<String>,
    },
    #[command(about = "Execute a command")]
    Exec,
    #[command(about = "List the tools")]
    List,
    #[command(about = "Show the path")]
    Path,
    #[command(about = "Add a tool")]
    Add {
        #[arg(
            short,
            long,
            help = "Source of the tool, example = \"github:evilmartians/lefthook\""
        )]
        source: String,
        #[arg(
            short,
            long,
            help = "Version of the tool, example = \"v2.1.10\" or \"2.1.10\""
        )]
        version: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match &cli.command {
        Command::Init => init::init(),
        Command::List => list::list(),
        Command::Update { tool } => update::update(tool.as_deref()),
        Command::Install => install::install(),
        Command::Exec => {
            println!("Executing...");
            Ok(())
        }
        Command::Path => {
            println!("Showing path...");
            Ok(())
        }
        Command::Add { source, version } => {
            println!("Adding tool {} with version {}", source, version);
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

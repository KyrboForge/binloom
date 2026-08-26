#[cfg(not(unix))]
compile_error!("binloom currently supports Unix-like systems only");
mod add;
mod common;
mod download;
mod exec;
#[cfg(test)]
mod http_fixture;
mod init;
mod install;
mod list;
mod lockfile;
mod manifest;
mod path;
mod platform;
mod resolve;
mod sources;
mod update;

use clap::{Parser, Subcommand};
use std::ffi::OsString;
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
    #[command(about = "Update tools and Binloom")]
    Update {
        /// Tool to update; omit to update all tools and Binloom
        tool: Option<String>,

        /// Update the locked Binloom binary
        #[arg(long = "self", conflicts_with = "tool")]
        update_self: bool,
    },
    #[command(about = "Execute a command with local tools available")]
    Exec {
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<OsString>,
    },
    #[command(about = "List the tools")]
    List,
    #[command(about = "Show the path")]
    Path,
    #[command(about = "Add a tool")]
    Add {
        /// Name used as the installed command
        name: String,
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
        #[arg(
            short,
            long,
            help = "Optional release asset pattern, for example tool_{version}_{os}_{arch}.gz"
        )]
        asset: Option<String>,
        version: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match &cli.command {
        Command::Init => init::init(),
        Command::List => list::list(),
        Command::Update { tool, update_self } => {
            if *update_self {
                update::update_binloom()
            } else {
                update::update(tool.as_deref())
            }
        }
        Command::Install => install::install(),
        Command::Exec { command } => exec::exec(command),
        Command::Path => path::path(),
        Command::Add {
            name,
            source,
            version,
            asset,
        } => add::add(name, source, version, asset.as_deref()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

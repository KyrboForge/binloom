#![warn(unreachable_pub)]

#[cfg(not(unix))]
compile_error!("binloom currently supports Unix-like systems only");
mod commands;
mod common;
mod domain;
mod download;
#[cfg(test)]
mod http_fixture;

use clap::{Parser, Subcommand};
use commands::{add, exec, init, install, list, path, update};
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
            help = "Source, for example github:evilmartians/lefthook or cargo:cargo-nextest"
        )]
        source: String,
        #[arg(
            short,
            long,
            help = "Version of the tool, example = \"v2.1.10\" or \"2.1.10\""
        )]
        version: String,
        #[arg(
            short,
            long,
            help = "Optional release asset pattern, for example tool_{version}_{os}_{arch}.gz"
        )]
        asset: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_add_patterns() {
        let cli = Cli::try_parse_from([
            "binloom",
            "add",
            "cargo-nextest",
            "--source",
            "github:nextest-rs/nextest",
            "--version",
            "0.9.143",
            "--asset",
            "cargo-nextest-{version}-{target}.tar.gz",
        ])
        .unwrap();

        let Command::Add { version, asset, .. } = cli.command else {
            panic!("expected add command");
        };

        assert_eq!(version, "0.9.143");
        assert_eq!(
            asset.as_deref(),
            Some("cargo-nextest-{version}-{target}.tar.gz")
        );
    }
}

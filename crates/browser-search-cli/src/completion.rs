use std::{
    io::{self, Write},
    process::ExitCode,
};

use browser_search_cli::Cli;
use clap::{Arg, Command, CommandFactory, value_parser};
use clap_complete::{
    Generator, Shell,
    env::{EnvCompleter, Fish},
};

const BIN: &str = "search";
pub(crate) const ENV_VAR: &str = "SEARCH_COMPLETE";

pub(crate) fn command() -> Command {
    Cli::command().subcommand(
        Command::new("completion")
            .about("Generate a shell completion script")
            .arg(
                Arg::new("shell")
                    .value_name("SHELL")
                    .required(true)
                    .value_parser(value_parser!(Shell)),
            ),
    )
}

pub(crate) fn run(shell: Shell) -> ExitCode {
    let mut stdout = io::stdout().lock();
    let result = match shell {
        Shell::PowerShell => stdout.write_all(include_bytes!("completion.ps1")),
        Shell::Elvish => stdout.write_all(include_bytes!("completion.elv")),
        Shell::Fish => Fish.write_registration(ENV_VAR, BIN, BIN, BIN, &mut stdout),
        _ => {
            let mut command = command().bin_name(BIN);
            command.build();
            shell.try_generate(&command, &mut stdout)
        }
    };
    if let Err(error) = result {
        eprintln!("failed to write completion script: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

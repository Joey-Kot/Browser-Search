mod completion;

use std::{
    io::{self, Write},
    process::ExitCode,
};

use browser_search_cli::{Cli, execute};
use clap::FromArgMatches;
use clap_complete::{CompleteEnv, Shell};

fn main() -> ExitCode {
    CompleteEnv::with_factory(completion::command)
        .var(completion::ENV_VAR)
        .complete();

    let mut matches = completion::command().get_matches();
    if let Some(args) = matches.subcommand_matches("completion") {
        let shell = *args.get_one::<Shell>("shell").expect("shell is required");
        return completion::run(shell);
    }

    let cli = Cli::from_arg_matches_mut(&mut matches).unwrap_or_else(|error| error.exit());
    search(cli)
}

#[tokio::main]
async fn search(cli: Cli) -> ExitCode {
    match execute(cli).await {
        Ok(results) => {
            let stdout = io::stdout();
            let mut output = stdout.lock();
            if let Err(error) = serde_json::to_writer(&mut output, &results)
                .and_then(|()| writeln!(output).map_err(serde_json::Error::io))
            {
                eprintln!("failed to write search results: {error}");
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit_code())
        }
    }
}

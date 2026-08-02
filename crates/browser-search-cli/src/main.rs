use std::{
    io::{self, Write},
    process::ExitCode,
};

use browser_search_cli::{Cli, execute};
use clap::Parser;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
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

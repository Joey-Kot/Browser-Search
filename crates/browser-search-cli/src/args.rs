use clap::{Args, Parser, Subcommand};

use crate::{error::CliError, model::SearchKind};

const AFTER_HELP: &str = r#"Environment:
  SEARCH_BASE_URL  Browser Search API base URL.
                   Optional. Default: http://127.0.0.1:17330

  SEARCH_API_KEY   Browser Search API key.
                   Required. Sent as: Authorization: Bearer <key>

Examples:
  search web --query "Tokyo" --search-num 100
  search news --query "OpenAI" --search-num 20
  search images --query "Tokyo skyline" --search-num 50"#;

#[derive(Debug, Clone, Parser)]
#[command(
    name = "search",
    version,
    about = "Search Google through a locally running Browser Search daemon",
    after_help = AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    command: SearchCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum SearchCommand {
    /// Search general web results.
    Web(SearchArgs),
    /// Search news results.
    News(SearchArgs),
    /// Search image results.
    Images(SearchArgs),
    /// Search video results.
    Videos(SearchArgs),
    /// Search forum results.
    Forums(SearchArgs),
}

impl SearchCommand {
    const fn kind(&self) -> SearchKind {
        match self {
            Self::Web(_) => SearchKind::Web,
            Self::News(_) => SearchKind::News,
            Self::Images(_) => SearchKind::Images,
            Self::Videos(_) => SearchKind::Videos,
            Self::Forums(_) => SearchKind::Forums,
        }
    }

    const fn args(&self) -> &SearchArgs {
        match self {
            Self::Web(args)
            | Self::News(args)
            | Self::Images(args)
            | Self::Videos(args)
            | Self::Forums(args) => args,
        }
    }
}

#[derive(Debug, Clone, Args)]
struct SearchArgs {
    /// Search keywords.
    #[arg(long, value_name = "TEXT")]
    query: String,

    /// Number of merged results to return.
    #[arg(
        long = "search-num",
        value_name = "COUNT",
        default_value_t = 10,
        value_parser = parse_search_num
    )]
    search_num: u32,

    /// Timeout for each CLI-to-daemon HTTP request, in seconds.
    /// Must be at least two seconds greater than --search-timeout.
    #[arg(long, value_name = "SECONDS", default_value_t = 120)]
    timeout: u64,

    /// Server-side timeout for each search page, in seconds.
    #[arg(long = "search-timeout", value_name = "SECONDS")]
    search_timeout: Option<u64>,
}

fn parse_search_num(value: &str) -> Result<u32, String> {
    let value = value
        .parse::<u32>()
        .map_err(|_| "must be an integer from 1 to 100".to_owned())?;
    if (1..=100).contains(&value) {
        Ok(value)
    } else {
        Err("must be an integer from 1 to 100".to_owned())
    }
}

impl Cli {
    pub(crate) fn resolve(&self) -> Result<ResolvedCli, CliError> {
        let kind = self.command.kind();
        let args = self.command.args();
        let query = args.query.trim().to_owned();
        if query.is_empty() {
            return Err(CliError::usage("--query must not be empty"));
        }
        if query.chars().count() > 512 {
            return Err(CliError::usage("--query must not exceed 512 characters"));
        }
        if args.timeout == 0 {
            return Err(CliError::usage("--timeout must be greater than 0"));
        }

        let search_timeout_ms = args
            .search_timeout
            .map(|seconds| {
                if seconds == 0 {
                    return Err(CliError::usage("--search-timeout must be greater than 0"));
                }
                if args.timeout <= seconds.saturating_add(1) {
                    return Err(CliError::usage(
                        "--timeout must be at least two seconds greater than --search-timeout",
                    ));
                }
                seconds.checked_mul(1_000).ok_or_else(|| {
                    CliError::usage("--search-timeout is too large to convert to milliseconds")
                })
            })
            .transpose()?;

        Ok(ResolvedCli {
            kind,
            query,
            search_num: args.search_num,
            timeout_seconds: args.timeout,
            search_timeout_ms,
        })
    }
}

#[derive(Debug)]
pub(crate) struct ResolvedCli {
    pub kind: SearchKind,
    pub query: String,
    pub search_num: u32,
    pub timeout_seconds: u64,
    pub search_timeout_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::Cli;

    #[test]
    fn help_lists_search_commands_environment_and_examples() {
        let help = Cli::command().render_long_help().to_string();
        for expected in [
            "Usage: search <COMMAND>",
            "web",
            "news",
            "images",
            "videos",
            "forums",
            "SEARCH_BASE_URL",
            "SEARCH_API_KEY",
            "search web --query \"Tokyo\" --search-num 100",
        ] {
            assert!(help.contains(expected), "missing help text: {expected}");
        }
    }

    #[test]
    fn resolves_shared_search_options() {
        let cli = Cli::try_parse_from([
            "search",
            "videos",
            "--query",
            "  Tokyo travel  ",
            "--search-num",
            "25",
            "--timeout",
            "45",
            "--search-timeout",
            "30",
        ])
        .unwrap();
        let resolved = cli.resolve().unwrap();
        assert_eq!(resolved.kind.as_str(), "videos");
        assert_eq!(resolved.query, "Tokyo travel");
        assert_eq!(resolved.search_num, 25);
        assert_eq!(resolved.timeout_seconds, 45);
        assert_eq!(resolved.search_timeout_ms, Some(30_000));
    }

    #[test]
    fn rejects_invalid_counts_and_timeouts() {
        for arguments in [
            vec!["search", "web", "--query", "x", "--search-num", "0"],
            vec!["search", "web", "--query", "x", "--search-num", "101"],
        ] {
            assert!(Cli::try_parse_from(arguments).is_err());
        }

        let zero_timeout =
            Cli::try_parse_from(["search", "web", "--query", "x", "--timeout", "0"]).unwrap();
        assert!(zero_timeout.resolve().is_err());

        let zero_search_timeout =
            Cli::try_parse_from(["search", "web", "--query", "x", "--search-timeout", "0"])
                .unwrap();
        assert!(zero_search_timeout.resolve().is_err());

        for timeout in ["120", "121"] {
            let conflicting_timeouts = Cli::try_parse_from([
                "search",
                "web",
                "--query",
                "x",
                "--timeout",
                timeout,
                "--search-timeout",
                "120",
            ])
            .unwrap();
            assert!(conflicting_timeouts.resolve().is_err());
        }

        let safe_timeouts = Cli::try_parse_from([
            "search",
            "web",
            "--query",
            "x",
            "--timeout",
            "122",
            "--search-timeout",
            "120",
        ])
        .unwrap();
        assert!(safe_timeouts.resolve().is_ok());
    }
}

use clap::{Args, CommandFactory, Parser, Subcommand};
use rdt_gateway_client::{Client, CommentOptions, DiscoveryOptions, ListOptions, SearchOptions};
use std::io::Write;
use tokio::io::AsyncReadExt;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(
    name = "rdt",
    version,
    color = clap::ColorChoice::Never,
    about = "Read public Reddit through rdt-gateway; results default to JSON"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        env = "RDT_GATEWAY_URL",
        default_value = "http://127.0.0.1:8787"
    )]
    url: String,
    /// Explicitly select machine-readable JSON (the default).
    #[arg(long, global = true, conflicts_with = "text")]
    json: bool,
    /// Render readable plain text instead of JSON.
    #[arg(long, global = true)]
    text: bool,
    /// Print the gateway origin and elapsed time to stderr.
    #[arg(long, global = true)]
    verbose: bool,
    /// Disable color (output is always uncolored).
    #[arg(long, global = true)]
    no_color: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Search public posts.
    Search {
        q: String,
        #[arg(long)]
        subreddit: Option<String>,
        #[arg(long, value_parser = ["relevance", "hot", "top", "new", "comments"])]
        sort: Option<String>,
        #[command(flatten)]
        listing: Listing,
    },
    /// List posts from a subreddit, all, or popular.
    Posts {
        name: String,
        #[arg(long, value_parser = ["hot", "new", "top", "rising", "controversial"])]
        sort: Option<String>,
        #[command(flatten)]
        listing: Listing,
    },
    /// Get a post. POST accepts an ID, t3_ID, Reddit URL, or '-' for stdin.
    Post {
        #[arg(value_name = "POST")]
        id: String,
    },
    /// Read a post and its bounded comment tree together.
    Read {
        #[arg(value_name = "POST")]
        id: String,
        #[command(flatten)]
        comments: CommentArgs,
    },
    /// Get a bounded comment tree. POST accepts an ID, URL, or '-' for stdin.
    Comments {
        #[arg(value_name = "POST")]
        id: String,
        #[command(flatten)]
        comments: CommentArgs,
    },
    /// Discover communities and read their public information.
    Subreddit {
        #[command(subcommand)]
        command: SubredditCommand,
    },
    /// Read public user profiles and history.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Print a shell completion script to stdout.
    Completions {
        #[arg(value_parser = ["bash", "zsh", "fish"])]
        shell: String,
    },
}

#[derive(Subcommand)]
enum SubredditCommand {
    Search {
        query: String,
        #[command(flatten)]
        discovery: Discovery,
    },
    Info {
        name: String,
    },
    Wiki {
        name: String,
        #[arg(long, default_value = "index")]
        page: String,
    },
    Rules {
        name: String,
    },
}

#[derive(Subcommand)]
enum UserCommand {
    Info {
        name: String,
    },
    Posts {
        name: String,
        #[arg(long, value_parser = ["hot", "new", "top", "controversial"])]
        sort: Option<String>,
        #[command(flatten)]
        listing: Listing,
    },
    Comments {
        name: String,
        #[arg(long, value_parser = ["hot", "new", "top", "controversial"])]
        sort: Option<String>,
        #[command(flatten)]
        listing: Listing,
    },
}

#[derive(Args)]
struct Listing {
    /// Time window: hour/day/week/month/year/all; only valid with compatible sorts.
    #[arg(long, value_parser = ["hour", "day", "week", "month", "year", "all"])]
    time: Option<String>,
    /// Maximum results (default 20); no automatic pagination.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    limit: Option<u32>,
    /// Opaque next_cursor returned by the previous page.
    #[arg(long)]
    cursor: Option<String>,
}
impl Listing {
    fn options(self, sort: Option<String>) -> ListOptions {
        ListOptions {
            sort,
            time: self.time,
            limit: self.limit,
            cursor: self.cursor,
        }
    }
}
#[derive(Args)]
struct Discovery {
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    limit: Option<u32>,
    #[arg(long)]
    cursor: Option<String>,
}
#[derive(Args)]
struct CommentArgs {
    /// Comment order.
    #[arg(long, value_parser = ["confidence", "top", "new", "controversial", "old", "qa"])]
    sort: Option<String>,
    /// Maximum depth (default 3); top-level comments have depth 1.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=8))]
    depth: Option<u32>,
    /// Maximum total returned comments (default 50).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=200))]
    limit: Option<u32>,
    /// Fetch omitted comments within the request budget.
    #[arg(long)]
    expand_more: bool,
    /// Request budget including the initial fetch (default 3); excludes authentication.
    #[arg(long, requires = "expand_more", value_parser = clap::value_parser!(u32).range(1..=10))]
    max_requests: Option<u32>,
}
impl From<CommentArgs> for CommentOptions {
    fn from(value: CommentArgs) -> Self {
        Self {
            sort: value.sort,
            depth: value.depth,
            limit: value.limit,
            expand_more: value.expand_more,
            max_requests: value.max_requests,
        }
    }
}

async fn post_input(id: String) -> Result<String> {
    if id != "-" {
        return Ok(id);
    }
    let mut input = String::new();
    tokio::io::stdin()
        .take(8193)
        .read_to_string(&mut input)
        .await?;
    let mut words = input.split_whitespace();
    let id = words.next().unwrap_or("");
    if input.len() > 8192 || id.is_empty() || words.next().is_some() {
        return Err(rdt_gateway_client::Error::InvalidArgument(
            "stdin must contain exactly one post ID or URL".into(),
        )
        .into());
    }
    Ok(id.to_owned())
}

impl Command {
    async fn execute(self, client: &Client) -> Result<serde_json::Value> {
        Ok(match self {
            Self::Search {
                q,
                subreddit,
                sort,
                listing,
            } => serde_json::to_value(
                client
                    .search(
                        &q,
                        SearchOptions {
                            subreddit,
                            sort,
                            time: listing.time,
                            limit: listing.limit,
                            cursor: listing.cursor,
                        },
                    )
                    .await?,
            )?,
            Self::Posts {
                name,
                sort,
                listing,
            } => serde_json::to_value(client.list_posts(&name, listing.options(sort)).await?)?,
            Self::Post { id } => serde_json::to_value(client.post(&post_input(id).await?).await?)?,
            Self::Read { id, comments } => {
                serde_json::to_value(client.read(&post_input(id).await?, comments.into()).await?)?
            }
            Self::Comments { id, comments } => serde_json::to_value(
                client
                    .comments(&post_input(id).await?, comments.into())
                    .await?,
            )?,
            Self::Subreddit { command } => match command {
                SubredditCommand::Search { query, discovery } => serde_json::to_value(
                    client
                        .search_subreddits(
                            &query,
                            DiscoveryOptions {
                                limit: discovery.limit,
                                cursor: discovery.cursor,
                            },
                        )
                        .await?,
                )?,
                SubredditCommand::Info { name } => {
                    serde_json::to_value(client.subreddit_info(&name).await?)?
                }
                SubredditCommand::Wiki { name, page } => {
                    serde_json::to_value(client.subreddit_wiki(&name, &page).await?)?
                }
                SubredditCommand::Rules { name } => {
                    serde_json::to_value(client.subreddit_rules(&name).await?)?
                }
            },
            Self::User { command } => match command {
                UserCommand::Info { name } => serde_json::to_value(client.user_info(&name).await?)?,
                UserCommand::Posts {
                    name,
                    sort,
                    listing,
                } => serde_json::to_value(client.user_posts(&name, listing.options(sort)).await?)?,
                UserCommand::Comments {
                    name,
                    sort,
                    listing,
                } => {
                    serde_json::to_value(client.user_comments(&name, listing.options(sort)).await?)?
                }
            },
            Self::Completions { .. } => unreachable!("completion is handled before connecting"),
        })
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    if let Some(long) = args.iter().find_map(|arg| {
        if arg == "--help" {
            Some(true)
        } else if arg == "-h" {
            Some(false)
        } else {
            None
        }
    }) {
        let mut command = Cli::command();
        command.build();
        let mut skip_url = false;
        for arg in &args[1..] {
            if skip_url {
                skip_url = false;
                continue;
            }
            if arg == "--url" {
                skip_url = true;
                continue;
            }
            let child = command
                .get_subcommands()
                .find(|child| arg == child.get_name())
                .cloned();
            if let Some(child) = child {
                command = child.color(clap::ColorChoice::Never);
            }
        }
        let result = if long {
            command.print_long_help()
        } else {
            command.print_help()
        };
        return if result.is_ok() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        };
    }
    let cli = Cli::parse_from(args);
    if let Command::Completions { shell } = &cli.command {
        clap_complete::generate(
            shell
                .parse::<clap_complete::Shell>()
                .expect("validated shell"),
            &mut Cli::command(),
            "rdt",
            &mut std::io::stdout(),
        );
        return std::process::ExitCode::SUCCESS;
    }
    let result = tokio::select! {
        result = run(cli) => result,
        signal = tokio::signal::ctrl_c() => {
            if let Err(error) = signal { eprintln!("rdt: {error}"); return std::process::ExitCode::FAILURE; }
            // Tokio stdin uses an uncancellable blocking read; exit promptly even with an open input pipe.
            std::process::exit(130);
        }
    };
    match result {
        Ok(partial) => std::process::ExitCode::from(u8::from(partial)),
        Err(error) => {
            eprintln!("rdt: {error}");
            std::process::ExitCode::from(
                if matches!(
                    error.downcast_ref::<rdt_gateway_client::Error>(),
                    Some(
                        rdt_gateway_client::Error::InvalidArgument(_)
                            | rdt_gateway_client::Error::InvalidBaseUrl
                    )
                ) {
                    2
                } else {
                    1
                },
            )
        }
    }
}

fn safe_text(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| {
            if ch.is_control() && ch != '\t' {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn render_text(
    value: &serde_json::Value,
    indent: usize,
    output: &mut impl Write,
) -> std::io::Result<()> {
    match value {
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if value.is_null() {
                    continue;
                }
                if value.is_object() || value.is_array() {
                    writeln!(output, "{:indent$}{key}:", "")?;
                    render_text(value, indent + 2, output)?;
                } else {
                    write!(output, "{:indent$}{key}: ", "")?;
                    render_text(value, 0, output)?;
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                render_text(value, indent, output)?;
                writeln!(output)?;
            }
        }
        serde_json::Value::String(value) => {
            for line in value.lines() {
                writeln!(output, "{:indent$}{}", "", safe_text(line))?;
            }
        }
        value => writeln!(output, "{:indent$}{value}", "")?,
    }
    Ok(())
}

async fn run(cli: Cli) -> Result<bool> {
    let client = Client::new(&cli.url)?;
    let start = std::time::Instant::now();
    if cli.verbose {
        // Never print credentials, paths, or query parameters from a gateway URL.
        let origin = url::Url::parse(&cli.url)?.origin().ascii_serialization();
        eprintln!("rdt: gateway {origin}");
    }
    let result = cli.command.execute(&client).await;
    if cli.verbose {
        eprintln!("rdt: elapsed {:.3}s", start.elapsed().as_secs_f64());
    }
    let value = result?;
    let partial = value
        .pointer("/meta/partial_error")
        .is_some_and(|value| !value.is_null());
    let mut stdout = std::io::stdout().lock();
    if cli.text {
        render_text(&value, 0, &mut stdout)?;
    } else {
        serde_json::to_writer(&mut stdout, &value)?;
        writeln!(stdout)?;
    }
    if partial {
        eprintln!("rdt: partial result; see meta.partial_error");
    }
    Ok(partial)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_escapes_terminal_controls() {
        let mut output = Vec::new();
        render_text(
            &serde_json::json!({"body": "hello\u{1b}[2J\nnext\tline"}),
            0,
            &mut output,
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains('\u{1b}'));
        assert!(output.contains("next\tline"));
    }
    #[test]
    fn command_tree_and_bounds() {
        Cli::command().debug_assert();
        for args in [
            vec!["rdt", "read", "abc", "--expand-more", "--max-requests", "3"],
            vec!["rdt", "subreddit", "wiki", "r/nixos"],
            vec!["rdt", "user", "comments", "u/test"],
            vec!["rdt", "completions", "zsh"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        for args in [
            vec!["rdt", "read", "abc", "--max-requests", "3"],
            vec!["rdt", "read", "abc", "--depth", "9"],
            vec!["rdt", "posts", "nixos", "--limit", "0"],
            vec!["rdt", "post", "abc", "--json", "--text"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
    #[tokio::test]
    async fn invalid_post_is_an_argument_error() {
        let cli = Cli::try_parse_from(["rdt", "post", "../search"]).unwrap();
        let error = cli
            .command
            .execute(&Client::new("http://127.0.0.1:1").unwrap())
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<rdt_gateway_client::Error>(),
            Some(rdt_gateway_client::Error::InvalidArgument(_))
        ));
    }
}

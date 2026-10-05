use clap::{Args, Parser, Subcommand};
use rdt_gateway_client::{Client, CommentOptions, ListOptions, SearchOptions};
use std::io::Write;

#[derive(Parser)]
#[command(
    version,
    about = "Read public Reddit through rdt-gateway; results are JSON"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        env = "RDT_GATEWAY_URL",
        default_value = "http://127.0.0.1:8787"
    )]
    url: String,
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
        #[command(flatten)]
        listing: Listing,
    },
    /// List a subreddit's posts.
    Posts {
        name: String,
        #[command(flatten)]
        listing: Listing,
    },
    /// Get a post by its bare base36 ID.
    Post { id: String },
    /// Get a bounded comment tree by post ID.
    Comments {
        id: String,
        #[arg(long)]
        sort: Option<String>,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=8))]
        depth: Option<u32>,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: Option<u32>,
    },
}

#[derive(Args)]
struct Listing {
    #[arg(long)]
    sort: Option<String>,
    #[arg(long)]
    time: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    limit: Option<u32>,
    #[arg(long)]
    cursor: Option<String>,
}

impl Command {
    async fn execute(
        self,
        client: &Client,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        Ok(match self {
            Self::Search {
                q,
                subreddit,
                listing,
            } => serde_json::to_value(
                client
                    .search(
                        &q,
                        SearchOptions {
                            subreddit,
                            sort: listing.sort,
                            time: listing.time,
                            limit: listing.limit,
                            cursor: listing.cursor,
                        },
                    )
                    .await?,
            )?,
            Self::Posts { name, listing } => serde_json::to_value(
                client
                    .list_posts(
                        &name,
                        ListOptions {
                            sort: listing.sort,
                            time: listing.time,
                            limit: listing.limit,
                            cursor: listing.cursor,
                        },
                    )
                    .await?,
            )?,
            Self::Post { id } => serde_json::to_value(client.post(&id).await?)?,
            Self::Comments {
                id,
                sort,
                depth,
                limit,
            } => serde_json::to_value(
                client
                    .comments(&id, CommentOptions { sort, depth, limit })
                    .await?,
            )?,
        })
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rdt-cli: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let value = cli.command.execute(&Client::new(&cli.url)?).await?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    writeln!(stdout)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_preserves_terms_and_cursor_for_http_encoding() {
        let cli = Cli::try_parse_from([
            "rdt-cli",
            "search",
            "nix & flakes",
            "--subreddit",
            "NixOS",
            "--limit",
            "10",
            "--cursor",
            "t3_abc",
        ])
        .unwrap();
        let Command::Search {
            q,
            subreddit,
            listing,
        } = cli.command
        else {
            panic!("expected search")
        };
        assert_eq!(q, "nix & flakes");
        assert_eq!(subreddit.as_deref(), Some("NixOS"));
        assert_eq!(listing.limit, Some(10));
        assert_eq!(listing.cursor.as_deref(), Some("t3_abc"));
    }

    #[test]
    fn defaults_are_owned_by_sdk_and_url_is_global() {
        let cli = Cli::try_parse_from([
            "rdt-cli",
            "comments",
            "abc123",
            "--url",
            "http://localhost:9000",
        ])
        .unwrap();
        assert_eq!(cli.url, "http://localhost:9000");
        let Command::Comments {
            sort, depth, limit, ..
        } = cli.command
        else {
            panic!("expected comments")
        };
        assert!(sort.is_none() && depth.is_none() && limit.is_none());
    }

    #[tokio::test]
    async fn rejects_paths_and_invalid_bounds() {
        let cli = Cli::try_parse_from(["rdt-cli", "post", "../search"]).unwrap();
        let error = cli
            .command
            .execute(&Client::new("http://127.0.0.1:1").unwrap())
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<rdt_gateway_client::Error>(),
            Some(rdt_gateway_client::Error::InvalidArgument(_))
        ));
        assert!(Cli::try_parse_from(["rdt-cli", "comments", "abc", "--depth", "9"]).is_err());
        assert!(Cli::try_parse_from(["rdt-cli", "posts", "nixos", "--limit", "0"]).is_err());
        assert!(Cli::try_parse_from(["rdt-cli", "search", "nix", "--unknown"]).is_err());
    }
}

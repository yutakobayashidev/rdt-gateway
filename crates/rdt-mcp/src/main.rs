use clap::Parser;
use rdt_gateway_client::{Client, Transport};
use rdt_mcp::{LocalTransport, Reddit};
use rmcp::{ServiceExt, transport::stdio};
use std::{net::SocketAddr, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    version,
    about = "Read public Reddit over MCP stdio, with an embedded gateway"
)]
struct Args {
    /// Use an external gateway instead of the embedded gateway.
    #[arg(long, env = "RDT_GATEWAY_URL", conflicts_with = "listen")]
    gateway_url: Option<String>,
    /// Also serve the embedded gateway's HTTP API on this address.
    #[arg(long)]
    listen: Option<SocketAddr>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rdt_gateway=info,rdt_mcp=info".into()),
        )
        .init();
    if let Some(url) = args.gateway_url {
        return run(Client::new(&url)?, None).await;
    }
    // Bind before initializing upstream so address errors fail immediately.
    let listener = match args.listen {
        Some(address) => Some(tokio::net::TcpListener::bind(address).await?),
        None => None,
    };
    let upstream = rdt_request::Upstream::new()
        .await
        .map_err(|error| format!("{}: {}", error.code, error.message))?;
    let state = rdt_gateway::api::State::new(upstream);
    let http = listener.map(|listener| (listener, state.clone()));
    run(Client::with_transport(LocalTransport::new(state)), http).await
}

async fn run<T: Transport>(
    client: Client<T>,
    http: Option<(tokio::net::TcpListener, rdt_gateway::api::State)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let shutdown = CancellationToken::new();
    let signal = signal();
    let mut http_task = http.map(|(listener, state)| {
        tracing::info!(address = %listener.local_addr().expect("bound listener"), "gateway listening");
        let token = shutdown.clone();
        tokio::spawn(async move {
            axum::serve(listener, rdt_gateway::api::router(state))
                .with_graceful_shutdown(token.cancelled_owned()).await
        })
    });
    let service = async {
        Reddit::new(client)
            .serve_with_ct(stdio(), shutdown.clone())
            .await?
            .waiting()
            .await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    tokio::pin!(service);
    let mut http_finished = false;
    let result = tokio::select! {
        result = &mut service => result,
        result = async {
            match &mut http_task {
                Some(task) => task.await,
                None => std::future::pending().await,
            }
        } => {
            http_finished = true;
            match result {
                Ok(Ok(())) => Err("HTTP gateway stopped unexpectedly".into()),
                Ok(Err(error)) => Err(error.into()),
                Err(error) => Err(error.into()),
            }
        },
        _ = signal => {
            shutdown.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut service).await;
            Ok(())
        }
    };
    shutdown.cancel();
    if let Some(mut task) = http_task.filter(|_| !http_finished) {
        match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
            Ok(result) => {
                result??;
            }
            Err(_) => {
                task.abort();
                let _ = task.await;
            }
        }
    }
    result
}

fn signal() -> impl std::future::Future<Output = ()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
        }
    }
    #[cfg(not(unix))]
    async {
        let _ = tokio::signal::ctrl_c().await;
    }
}

use rdt_gateway::api;
use rdt_request::Upstream;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rdt_gateway=info".into()),
        )
        .init();
    let mut listen =
        std::env::var("RDT_GATEWAY_LISTEN").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = args.next().ok_or("--listen requires an address:port")?,
            "--help" | "-h" => {
                println!("rdt-gateway [--listen ADDRESS:PORT]\n\nRead-only HTTP daemon. Default: 127.0.0.1:8787\nEnvironment: RDT_GATEWAY_LISTEN, RUST_LOG");
                return Ok(());
            }
            "--version" | "-V" => {
                println!("rdt-gateway {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => return Err(format!("Unknown argument: {arg}").into()),
        }
    }
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    let upstream = Upstream::new()
        .await
        .map_err(|e| format!("{}: {}", e.code, e.message))?;
    tracing::info!(address = %listener.local_addr()?, "gateway listening");
    axum::serve(listener, api::router(api::State::new(upstream)))
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("install SIGTERM handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await?;
    Ok(())
}

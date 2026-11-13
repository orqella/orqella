use anyhow::Result;
use clap::Parser;
use orqella_desktop_gateway::{Gateway, GatewayConfig, Upstream};
use std::net::IpAddr;

#[derive(Debug, Parser)]
#[command(name = "orqella-desktop-gateway")]
struct Args {
    /// Address the gateway binds to.
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,
    /// Gateway port. 0 chooses an ephemeral port.
    #[arg(long, default_value_t = 19477)]
    port: u16,
    /// Address of the hub or local tunnel.
    #[arg(long, default_value = "127.0.0.1")]
    upstream_host: String,
    /// Hub port at the upstream address.
    #[arg(long, default_value_t = 9477)]
    upstream_port: u16,
    /// Canonical host the hub expects.
    #[arg(long, default_value = "orqella.localhost")]
    canonical_host: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "orqella_desktop_gateway=info".into()),
        )
        .init();
    let args = Args::parse();
    if !args.bind.is_loopback() {
        anyhow::bail!("the gateway must bind to a loopback address");
    }
    let gateway = Gateway::start(GatewayConfig {
        bind: args.bind,
        port: args.port,
        shell_host: args.canonical_host.clone(),
        upstream: Upstream::http(args.canonical_host, args.upstream_host, args.upstream_port),
        app_origins: Vec::new(),
        origin_namespace: 0,
    })
    .await?;
    println!("{}", gateway.base_url);
    tokio::signal::ctrl_c().await?;
    gateway.stop().await;
    Ok(())
}

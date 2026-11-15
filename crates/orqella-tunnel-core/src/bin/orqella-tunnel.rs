use anyhow::{Context, Result};
use clap::Parser;
use orqella_tunnel_core::{TunnelConfig, start};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "orqella-tunnel",
    about = "Foreground SSH tunnel harness for Orqella"
)]
struct Cli {
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 22)]
    port: u16,
    #[arg(long)]
    user: String,
    #[arg(long)]
    key: PathBuf,
    #[arg(long)]
    fingerprint: String,
    #[arg(long, default_value = "127.0.0.1")]
    remote_host: String,
    #[arg(long, default_value_t = 9477)]
    remote_port: u16,
    #[arg(long, default_value_t = 0)]
    local_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let tunnel = start(TunnelConfig {
        host: cli.host,
        port: cli.port,
        user: cli.user,
        private_key: cli.key,
        key_passphrase: None,
        expected_host_fingerprint: Some(cli.fingerprint),
        remote_host: cli.remote_host,
        remote_port: cli.remote_port,
        local_port: cli.local_port,
        local_token: None,
    })
    .await?;
    println!("http://{}", tunnel.local_addr);
    tokio::signal::ctrl_c()
        .await
        .context("signal handler failed")?;
    tunnel.stop().await;
    Ok(())
}

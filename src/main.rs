mod utils;
mod errors;
mod config;
mod server;
mod client;
mod daemon;
mod crypto;
mod clipboard;
mod synq {
    tonic::include_proto!("synq");
}

use clap::Parser;
use crate::errors::{Result};
use crate::config::{Config};

#[derive(Parser, Debug)]
#[command(name = "synq")]
#[command(about = "Synq - TODO")]
struct Args {
    #[arg(long)]
    daemon: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();

    if args.daemon {
        let config = Config::load("/home/cloud/.config/synq.conf").await?;
        if config.is_modified() {
            config.save("/home/cloud/.config/synq.conf").await?;
        }

        daemon::run(config).await?
    }

    Ok(())
}

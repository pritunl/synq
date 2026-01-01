mod utils;
mod errors;
mod config;
mod server;
mod client;
mod synq {
    tonic::include_proto!("synq");
}

use clap::Parser;
use crate::errors::{Result};
use crate::server::{Server};
use crate::client::{Client};
use crate::config::{Config};

#[derive(Parser, Debug)]
#[command(name = "synq")]
#[command(about = "Synq - TODO")]
struct Args {
    #[arg(long, conflicts_with = "server")]
    client: bool,

    #[arg(long, conflicts_with = "client")]
    server: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();
    let config = Config::load("/home/cloud/.config/synq.conf").await?;

    if args.client {
        println!("Starting in client mode");
        println!("Public Key: {}", config.server.public_key);

        let peers: Vec<String> = config.peers.iter()
            .map(|p| p.address.clone())
            .collect();

        for peer in &peers {
            println!("Peer: {}", peer);
        }

        Client::run(config.server.public_key.clone(), peers).await?;
    } else if args.server {
        println!("Starting in server mode");
        println!("Bind: {}", config.server.bind);
        println!("Public Key: {}", config.server.public_key);

        Server::run(config.server.bind.clone()).await?;
    } else {
        eprintln!("Error: Must specify either --client or --server");
        std::process::exit(1);
    }

    Ok(())
}

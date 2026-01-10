mod utils;
mod errors;
mod config;
mod daemon;
mod crypto;
mod scroll;
mod clipboard;
mod synq;

use clap::{Parser, Subcommand};
use crate::errors::Result;
use crate::config::Config;

#[derive(Parser, Debug)]
#[command(name = "synq")]
#[command(about = "Synq - TODO")]
struct Args {
    #[arg(long)]
    daemon: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    ListDevices,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();

    if let Some(command) = args.command {
        match command {
            Command::ListDevices => {
                let devices = scroll::list_devices()?;
                for device in devices {
                    println!("{}", device);
                }
            }
        }
        return Ok(());
    }

    if args.daemon {
        let config = Config::load("/home/cloud/.config/synq.conf").await?;
        if config.is_modified() {
            config.save("/home/cloud/.config/synq.conf").await?;
        }

        daemon::run(config).await?
    }

    Ok(())
}

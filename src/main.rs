mod utils;
mod errors;
mod config;
mod daemon;
mod crypto;
mod scroll;
mod clipboard;
mod synq;
mod transport;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use crate::errors::{Result, Error, ErrorKind};
use crate::config::Config;

#[derive(Parser, Debug)]
#[command(name = "synq")]
#[command(about = "Synq - Clipboard and scroll wheel sharing")]
struct Args {
    #[arg(long)]
    debug: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    Daemon,
    ListDevices,
    DetectDevices,
}

fn get_config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|e| Error::wrap(e, ErrorKind::Parse)
            .with_msg("main: Failed to get HOME environment variable"))?;

    Ok(PathBuf::from(home).join(".config/synq.conf"))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();

    if args.debug {
        errors::set_debug_output(true);
    }

    if let Some(command) = args.command {
        match command {
            Command::Daemon => {
                let config_path = get_config_path()?;
                let config = Config::load(&config_path).await?;
                if config.is_modified() {
                    config.save().await?;
                }

                daemon::run(config).await?;
            }
            Command::ListDevices => {
                let devices = scroll::list_devices()?;
                for device in devices {
                    println!("{}", device);
                }
            }
            Command::DetectDevices => {
                let config_path = get_config_path()?;
                let config = Config::load(&config_path).await?;
                if config.is_modified() {
                    config.save().await?;
                }

                scroll::detect_scroll_devices(config).await?;
            }
        }
    }

    Ok(())
}

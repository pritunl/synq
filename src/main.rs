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
use crate::errors::Result;
use crate::config::Config;
use crate::utils::get_config_path;

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
